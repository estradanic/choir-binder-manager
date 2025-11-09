//! Ratatui front-end for the Choir Binder Manager. This file is intentionally
//! verbose: it records not just *what* each UI state does but also *why* the
//! interactions behave the way they do. The extra detail preserves the
//! reasoning behind shortcuts, layout decisions, and the "To Print" flow for
//! future maintenance.

use std::cmp::min;
use std::collections::HashSet;
use std::io::{self, Stdout};
use std::mem;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use open::that as open_link;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use rusqlite::Connection;

use crate::db::{
    add_song_to_binder, create_binder, create_song, delete_binder, delete_song, fetch_all_songs,
    fetch_available_songs, fetch_binders, fetch_composers, fetch_songs_for_binder,
    remove_song_from_binder, update_binder, update_song,
};
use crate::models::{Binder, Song};

/// Number of binder cards shown in each row of the main grid. Four columns are
/// a sweet spot on most terminal sizes while keeping text legible.
const GRID_COLUMNS: usize = 4;
/// Footer space reserved for status messages and instructions.
const FOOTER_HEIGHT: u16 = 3;
/// Height allocation per song card in list-style views.
const SONG_CARD_HEIGHT: u16 = 5;
/// ASCII textures used to decorate binder covers. We rotate through the list so
/// large collections feel more playful without needing color support.
const BINDER_ART: &[&[&str]] = &[
    &["/\\/\\/", "\\/\\/\\"],
    &["*+*+", "+*+*"],
    &["=--=", "--=="],
    &["<>><", "><<>"],
    &["..--", "--.."],
    &["oOo ", " OoO"],
    &["##  ", "  ##"],
    &["||--", "--||"],
    &["[]__", "__[]"],
    &["~~  ", "  ~~"],
    &["^v^v", "v^v^"],
    &["&&..", "..&&"],
    &["::''", "''::"],
    &["+-+-", "-+-+"],
    &["ooOO", "OOoo"],
    &["[]<>", "<>[]"],
    &["/--/", "--//"],
    &["=__=", "__=="],
    &["|..|", ".||."],
    &["x  x", "  xx"],
];

/// Repeat a short ASCII motif until it fills the requested width. The extra
/// padding in `repeat_count` ensures even narrow patterns stay seamless after
/// terminal resizes.
fn repeat_pattern_row(row: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if row.is_empty() {
        return " ".repeat(width);
    }
    let repeat_count = width / row.len() + 2;
    let mut repeated = row.repeat(repeat_count);
    repeated.truncate(width);
    repeated
}

/// Render the binder label centered inside square brackets. This helper keeps
/// the truncation and padding consistent for every view that shows a binder
/// label overlay.
fn binder_label_line(label: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return " ".repeat(width);
    }
    let mut decorated = format!("[ {} ]", trimmed);
    if decorated.len() > width {
        decorated.truncate(width);
    }
    let padding = width.saturating_sub(decorated.len());
    let left = padding / 2;
    let right = padding - left;
    let mut line = String::with_capacity(width);
    line.push_str(&" ".repeat(left));
    line.push_str(&decorated);
    line.push_str(&" ".repeat(right));
    if line.len() < width {
        line.push_str(&" ".repeat(width - line.len()));
    } else if line.len() > width {
        line.truncate(width);
    }
    line
}

/// Build the textual payload for a binder card, mixing the repeating pattern
/// with an optional bold highlight when the card is selected.
fn build_binder_cover_lines(
    binder: &Binder,
    pattern: &[&str],
    inner_width: u16,
    inner_height: u16,
    selected: bool,
) -> Vec<Line<'static>> {
    let width = inner_width as usize;
    let height = inner_height as usize;
    if width == 0 || height == 0 {
        return vec![Line::from("")];
    }

    let mut lines = Vec::with_capacity(height);
    let pattern_rows = pattern.len();
    let label_lines = if height >= 2 { 2 } else { 1 };
    let pattern_height = height.saturating_sub(label_lines);
    let pattern_style = if selected {
        Style::default().fg(Color::Gray)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    if pattern_rows == 0 {
        for _ in 0..pattern_height {
            lines.push(Line::from(vec![Span::styled(
                " ".repeat(width),
                pattern_style,
            )]));
        }
    } else {
        for row_idx in 0..pattern_height {
            let base = pattern[row_idx % pattern_rows];
            let row = repeat_pattern_row(base, width);
            lines.push(Line::from(vec![Span::styled(row, pattern_style)]));
        }
    }

    if height >= 2 {
        lines.push(Line::from(vec![Span::styled(
            " ".repeat(width),
            pattern_style,
        )]));
    }

    let label_content = binder_label_line(&binder.label, width);
    if selected {
        lines.push(Line::from(vec![Span::styled(
            label_content,
            Style::default().add_modifier(Modifier::BOLD),
        )]));
    } else {
        lines.push(Line::from(label_content));
    }

    while lines.len() < height {
        lines.push(Line::from(vec![Span::styled(
            " ".repeat(width),
            pattern_style,
        )]));
    }

    lines
}

/// High-level navigation states. Keeping this explicit makes it easy to reason
/// about which rendering path runs and what keyboard shortcuts should do.
enum Screen {
    Binders,
    Songs(SongScreen),
    SongManager(SongManagerScreen),
    ToPrint(ToPrintScreen),
}

/// Fine-grained modes scoped to the current screen. Many interactions borrow
/// from Vim-style modal flows (Normal vs. form entry vs. confirmation) so we
/// can keep the keyboard model predictable.
enum Mode {
    Normal,
    AddingBinder(BinderForm),
    EditingBinder {
        id: i64,
        form: BinderForm,
    },
    ConfirmBinderDelete(ConfirmBinderDelete),
    EditingSong {
        song_id: i64,
        form: SongForm,
    },
    ConfirmSongRemove(ConfirmSongRemove),
    SelectingSong(AddSongState),
    ConfirmSongDelete(ConfirmSongDelete),
    CreatingSong {
        binder_id: Option<i64>,
        form: SongForm,
    },
    ConfirmToPrintExit(ConfirmToPrintExit),
    /// Search mode: typing updates the query and filters the current song list
    Searching(SearchState),
}

/// Which screen the search is targeting.
enum SearchTarget {
    Songs,
    SongManager,
}

/// State for an active inline search. `query` is the current text shown in
/// the search bar. `target` selects which list will be filtered.
struct SearchState {
    target: SearchTarget,
    query: String,
}

/// Central application state shared across the TUI. The struct combines the
/// persistent connection, in-memory caches, and the active mode.
pub struct App {
    /// Long-lived SQLite connection. We keep it on the struct so every handler
    /// can synchronously issue queries without extra plumbing.
    conn: Connection,
    /// Copy of all binders currently loaded. This is mutated locally whenever
    /// the user creates, edits, or deletes binders.
    binders: Vec<Binder>,
    /// Index of the selected binder in the grid (zero-based).
    selected: usize,
    /// Distinct composers cached for auto-complete.
    composers: Vec<String>,
    /// Active high-level screen.
    screen: Screen,
    /// Current interaction mode for that screen.
    mode: Mode,
    /// Optional status line surfaced in the footer.
    status: Option<StatusMessage>,
    /// When a search is interrupted by opening a modal (edit), we stash the
    /// SearchState here so it can be restored after the modal closes.
    saved_search: Option<SearchState>,
}

impl App {
    /// Construct a new `App` with the preloaded binders and composers. We store
    /// the provided connection directly so subsequent actions can hit the
    /// database without re-establishing a connection.
    pub fn new(conn: Connection, binders: Vec<Binder>, composers: Vec<String>) -> Self {
        Self {
            conn,
            binders,
            selected: 0,
            composers,
            screen: Screen::Binders,
            mode: Mode::Normal,
            status: None,
            saved_search: None,
        }
    }

    /// Top-level key dispatcher. The design funnels every key through the
    /// active `Mode`, which returns the next mode to run. The boolean result
    /// tells the outer loop whether the user requested an exit.
    pub fn handle_key(&mut self, code: KeyCode) -> Result<bool> {
        let mut exit = false;
        let mut mode = mem::replace(&mut self.mode, Mode::Normal);

        mode = match mode {
            Mode::Normal => self.handle_normal_key(code, &mut exit)?,
            Mode::AddingBinder(form) => self.handle_add_binder(code, form)?,
            Mode::EditingBinder { id, form } => self.handle_edit_binder(code, id, form)?,
            Mode::ConfirmBinderDelete(confirm) => {
                self.handle_confirm_binder_delete(code, confirm)?
            }
            Mode::EditingSong { song_id, form } => self.handle_edit_song(code, song_id, form)?,
            Mode::ConfirmSongRemove(confirm) => self.handle_confirm_song_remove(code, confirm)?,
            Mode::ConfirmSongDelete(confirm) => self.handle_confirm_song_delete(code, confirm)?,
            Mode::SelectingSong(state) => self.handle_select_song(code, state)?,
            Mode::CreatingSong { binder_id, form } => {
                self.handle_create_song(code, binder_id, form)?
            }
            Mode::ConfirmToPrintExit(confirm) => {
                self.handle_confirm_to_print_exit(code, confirm, &mut exit)?
            }
            Mode::Searching(state) => self.handle_search(code, state)?,
        };

        self.mode = mode;
        Ok(exit)
    }

    /// Handle keys while in `Mode::Normal`. This branch performs most of the
    /// navigation work (moving around the binder grid, opening sub-views, etc.)
    /// and returns the next mode the application should switch to.
    fn handle_normal_key(&mut self, code: KeyCode, exit: &mut bool) -> Result<Mode> {
        match self.screen {
            Screen::Binders => {
                match code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        *exit = true;
                    }
                    KeyCode::Left => self.move_horizontal(-1),
                    KeyCode::Right => self.move_horizontal(1),
                    KeyCode::Up => self.move_vertical(-1),
                    KeyCode::Down => self.move_vertical(1),
                    KeyCode::Enter => {
                        if let Some(binder) = self.current_binder().cloned() {
                            self.open_binder_view(binder)?;
                        } else {
                            self.set_status("No binder selected.", StatusKind::Error);
                        }
                    }
                    KeyCode::Char('s') | KeyCode::Char('S') => {
                        self.clear_status();
                        self.open_song_manager()?;
                    }
                    KeyCode::Char('p') | KeyCode::Char('P') => {
                        self.clear_status();
                        self.open_to_print_view()?;
                    }
                    KeyCode::Char('+') => {
                        self.clear_status();
                        let mut form = BinderForm::with_number(self.next_binder_number());
                        form.focus(BinderField::Number);
                        return Ok(Mode::AddingBinder(form));
                    }
                    KeyCode::Char('-') => {
                        if let Some(binder) = self.current_binder().cloned() {
                            self.clear_status();
                            return Ok(Mode::ConfirmBinderDelete(ConfirmBinderDelete::from(
                                binder,
                            )));
                        } else {
                            self.set_status("No binder selected to remove.", StatusKind::Error);
                        }
                    }
                    KeyCode::Char('e') | KeyCode::Char('E') => {
                        if let Some(binder) = self.current_binder().cloned() {
                            self.clear_status();
                            return Ok(Mode::EditingBinder {
                                id: binder.id,
                                form: BinderForm::from_binder(&binder),
                            });
                        } else {
                            self.set_status("No binder selected to edit.", StatusKind::Error);
                        }
                    }
                    _ => {}
                }
                Ok(Mode::Normal)
            }
            Screen::Songs(ref mut songs) => {
                let mut status_to_set: Option<(String, StatusKind)> = None;
                let mut clear_status = false;
                let mut switch_to_binders = false;
                let mut open_manager = false;
                let mut open_to_print = false;

                {
                    let songs = &mut *songs;
                    match code {
                        KeyCode::Char('q') => {
                            *exit = true;
                        }
                        KeyCode::Esc => {
                            switch_to_binders = true;
                            clear_status = true;
                        }
                        KeyCode::Up => songs.move_selection(-1),
                        KeyCode::Down => songs.move_selection(1),
                        KeyCode::PageUp => songs.move_selection(-5),
                        KeyCode::PageDown => songs.move_selection(5),
                        KeyCode::Home => songs.select_first(),
                        KeyCode::End => songs.select_last(),
                        KeyCode::Char('f') => {
                            return Ok(Mode::Searching(SearchState {
                                target: SearchTarget::Songs,
                                query: String::new(),
                            }));
                        }
                        KeyCode::Char('s') | KeyCode::Char('S') => {
                            open_manager = true;
                        }
                        KeyCode::Char('p') | KeyCode::Char('P') => {
                            open_to_print = true;
                        }
                        KeyCode::Tab => {
                            self.clear_status();
                            self.open_relative_binder(1)?;
                        }
                        KeyCode::BackTab => {
                            self.clear_status();
                            self.open_relative_binder(-1)?;
                        }
                        KeyCode::Enter => {
                            if let Some(song) = songs.current_song().cloned() {
                                let link = song.link.trim().to_string();
                                if link.is_empty() {
                                    status_to_set = Some((
                                        "This song does not have a link.".to_string(),
                                        StatusKind::Error,
                                    ));
                                } else if let Err(err) = open_link(&link) {
                                    status_to_set = Some((
                                        format!("Failed to open link: {err}"),
                                        StatusKind::Error,
                                    ));
                                } else {
                                    status_to_set = Some((
                                        format!("Opened {}.", song.display_title()),
                                        StatusKind::Info,
                                    ));
                                }
                            }
                        }
                        KeyCode::Char('+') => {
                            if let Some(binder_id) = songs.binder_id() {
                                let state = AddSongState::load(&self.conn, binder_id)?;
                                if state.len() == 1 {
                                    let form = SongForm::default();
                                    return Ok(Mode::CreatingSong {
                                        binder_id: Some(binder_id),
                                        form,
                                    });
                                }
                                return Ok(Mode::SelectingSong(state));
                            }
                        }
                        KeyCode::Char('-') => {
                            if let Some(song) = songs.current_song().cloned() {
                                let binder_id = songs.binder_id().unwrap();
                                return Ok(Mode::ConfirmSongRemove(ConfirmSongRemove {
                                    binder_id,
                                    song,
                                }));
                            } else {
                                status_to_set = Some((
                                    "No song selected to remove.".to_string(),
                                    StatusKind::Error,
                                ));
                            }
                        }
                        KeyCode::Char('e') | KeyCode::Char('E') => {
                            if let Some(song) = songs.current_song().cloned() {
                                return Ok(Mode::EditingSong {
                                    song_id: song.id,
                                    form: SongForm::from_song(&song),
                                });
                            } else {
                                status_to_set = Some((
                                    "No song selected to edit.".to_string(),
                                    StatusKind::Error,
                                ));
                            }
                        }
                        _ => {}
                    }
                }

                if switch_to_binders {
                    self.screen = Screen::Binders;
                } else if open_manager {
                    self.open_song_manager()?;
                } else if open_to_print {
                    self.open_to_print_view()?;
                }

                if clear_status {
                    self.clear_status();
                } else if let Some((text, kind)) = status_to_set {
                    self.set_status(text, kind);
                }

                Ok(Mode::Normal)
            }
            Screen::SongManager(ref mut manager) => {
                let mut status_to_set: Option<(String, StatusKind)> = None;
                let mut return_to_binders = false;
                let mut open_to_print = false;
                let mut toggled_no_link: Option<bool> = None;

                {
                    let manager = &mut *manager;
                    match code {
                        KeyCode::Char('q') => {
                            *exit = true;
                        }
                        KeyCode::Esc | KeyCode::Char('s') | KeyCode::Char('S') => {
                            return_to_binders = true;
                        }
                        KeyCode::Char('f') => {
                            return Ok(Mode::Searching(SearchState {
                                target: SearchTarget::SongManager,
                                query: String::new(),
                            }));
                        }
                        KeyCode::Up => manager.move_selection(-1),
                        KeyCode::Down => manager.move_selection(1),
                        KeyCode::PageUp => manager.move_selection(-5),
                        KeyCode::PageDown => manager.move_selection(5),
                        KeyCode::Home => manager.select_first(),
                        KeyCode::End => manager.select_last(),
                        KeyCode::Enter => {
                            if let Some(song) = manager.current_song().cloned() {
                                let link = song.link.trim().to_string();
                                if link.is_empty() {
                                    status_to_set = Some((
                                        "This song does not have a link.".to_string(),
                                        StatusKind::Error,
                                    ));
                                } else if let Err(err) = open_link(&link) {
                                    status_to_set = Some((
                                        format!("Failed to open link: {err}"),
                                        StatusKind::Error,
                                    ));
                                } else {
                                    status_to_set = Some((
                                        format!("Opened {}.", song.display_title()),
                                        StatusKind::Info,
                                    ));
                                }
                            }
                        }
                        KeyCode::Char('+') => {
                            let form = SongForm::default();
                            return Ok(Mode::CreatingSong {
                                binder_id: None,
                                form,
                            });
                        }
                        KeyCode::Char('-') => {
                            if let Some(song) = manager.current_song().cloned() {
                                return Ok(Mode::ConfirmSongDelete(ConfirmSongDelete { song }));
                            } else {
                                status_to_set = Some((
                                    "No song selected to delete.".to_string(),
                                    StatusKind::Error,
                                ));
                            }
                        }
                        KeyCode::Char('e') | KeyCode::Char('E') => {
                            if let Some(song) = manager.current_song().cloned() {
                                return Ok(Mode::EditingSong {
                                    song_id: song.id,
                                    form: SongForm::from_song(&song),
                                });
                            } else {
                                status_to_set = Some((
                                    "No song selected to edit.".to_string(),
                                    StatusKind::Error,
                                ));
                            }
                        }
                        KeyCode::Char('p') | KeyCode::Char('P') => {
                            open_to_print = true;
                        }
                        KeyCode::Char('l') | KeyCode::Char('L') => {
                            toggled_no_link = Some(manager.toggle_show_no_link());
                        }
                        _ => {}
                    }
                }

                if return_to_binders {
                    self.clear_status();
                    self.screen = Screen::Binders;
                } else if open_to_print {
                    self.open_to_print_view()?;
                } else if let Some(active) = toggled_no_link {
                    let message = if active {
                        "Showing songs without links.".to_string()
                    } else {
                        "Showing all songs.".to_string()
                    };
                    self.set_status(message, StatusKind::Info);
                } else if let Some((text, kind)) = status_to_set {
                    self.set_status(text, kind);
                }

                Ok(Mode::Normal)
            }
            Screen::ToPrint(ref mut report) => {
                match code {
                    KeyCode::Char('q') => {
                        if report.has_pending_changes() {
                            return Ok(Mode::ConfirmToPrintExit(ConfirmToPrintExit::new(true)));
                        }
                        *exit = true;
                    }
                    KeyCode::Esc | KeyCode::Char('p') | KeyCode::Char('P') => {
                        if report.has_pending_changes() {
                            return Ok(Mode::ConfirmToPrintExit(ConfirmToPrintExit::new(false)));
                        }
                        self.clear_status();
                        self.screen = Screen::Binders;
                    }
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('t') | KeyCode::Char('T') => {
                        report.toggle_mode();
                    }
                    KeyCode::Up => report.move_selection(-1),
                    KeyCode::Down => report.move_selection(1),
                    KeyCode::PageUp => report.move_selection(-5),
                    KeyCode::PageDown => report.move_selection(5),
                    KeyCode::Home => report.select_first(),
                    KeyCode::End => report.select_last(),
                    KeyCode::Char(' ') => {
                        if let Some(checked) = report.toggle_current() {
                            if checked {
                                self.set_status("Marked song as added.", StatusKind::Info);
                            } else {
                                self.set_status("Song unchecked.", StatusKind::Info);
                            }
                        }
                    }
                    _ => {}
                }
                Ok(Mode::Normal)
            }
        }
    }

    /// Process key presses while the "Add Binder" form is active. Returns the
    /// next mode so the caller can continue driving the state machine.
    fn handle_add_binder(&mut self, code: KeyCode, mut form: BinderForm) -> Result<Mode> {
        let mut keep_open = true;
        match code {
            KeyCode::Esc => {
                self.set_status("Add binder cancelled.", StatusKind::Info);
                keep_open = false;
            }
            KeyCode::Tab | KeyCode::BackTab => form.toggle_field(),
            KeyCode::Backspace => form.backspace(),
            KeyCode::Enter => match self.save_new_binder(&form) {
                Ok(_) => keep_open = false,
                Err(err) => {
                    let message = surface_error(&err);
                    form.error = Some(message.clone());
                    self.set_status(message, StatusKind::Error);
                }
            },
            KeyCode::Char(ch) => {
                if form.push_char(ch) {
                    form.error = None;
                }
            }
            _ => {}
        }

        if keep_open {
            Ok(Mode::AddingBinder(form))
        } else {
            Ok(Mode::Normal)
        }
    }

    /// Mirror of `handle_add_binder` for edits, keeping the binder id intact so
    /// we can persist updates.
    fn handle_edit_binder(&mut self, code: KeyCode, id: i64, mut form: BinderForm) -> Result<Mode> {
        let mut keep_open = true;
        match code {
            KeyCode::Esc => {
                self.set_status("Edit cancelled.", StatusKind::Info);
                keep_open = false;
            }
            KeyCode::Tab | KeyCode::BackTab => form.toggle_field(),
            KeyCode::Backspace => form.backspace(),
            KeyCode::Enter => match self.save_existing_binder(id, &form) {
                Ok(_) => keep_open = false,
                Err(err) => {
                    let message = surface_error(&err);
                    form.error = Some(message.clone());
                    self.set_status(message, StatusKind::Error);
                }
            },
            KeyCode::Char(ch) => {
                if form.push_char(ch) {
                    form.error = None;
                }
            }
            _ => {}
        }

        if keep_open {
            Ok(Mode::EditingBinder { id, form })
        } else {
            Ok(Mode::Normal)
        }
    }

    /// Confirmation dialog for binder deletion. Escape cancels, enter confirms.
    fn handle_confirm_binder_delete(
        &mut self,
        code: KeyCode,
        confirm: ConfirmBinderDelete,
    ) -> Result<Mode> {
        match code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.set_status("Deletion cancelled.", StatusKind::Info);
                Ok(Mode::Normal)
            }
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                match self.perform_delete(&confirm) {
                    Ok(_) => Ok(Mode::Normal),
                    Err(err) => {
                        let message = surface_error(&err);
                        self.set_status(message, StatusKind::Error);
                        Ok(Mode::ConfirmBinderDelete(confirm))
                    }
                }
            }
            _ => Ok(Mode::ConfirmBinderDelete(confirm)),
        }
    }

    /// Song editing form handler, including support for composer autocomplete.
    fn handle_edit_song(
        &mut self,
        code: KeyCode,
        song_id: i64,
        mut form: SongForm,
    ) -> Result<Mode> {
        let mut keep_open = true;
        match code {
            KeyCode::Esc => {
                if !form.cancel_autocomplete() {
                    self.set_status("Edit cancelled.", StatusKind::Info);
                    keep_open = false;
                }
            }
            KeyCode::Tab => {
                let consumed = form.has_active_suggestion() && form.accept_suggestion();
                if !consumed {
                    form.toggle_field();
                }
                form.update_suggestion(&self.composers);
            }
            KeyCode::BackTab => {
                form.toggle_field();
                form.update_suggestion(&self.composers);
            }
            KeyCode::Backspace => {
                form.backspace();
                form.update_suggestion(&self.composers);
            }
            KeyCode::Enter => match form.parse_inputs() {
                Ok((title, composer, link)) => {
                    if let Err(err) = update_song(&self.conn, song_id, &title, &composer, &link) {
                        let message = surface_error(&err);
                        form.error = Some(message.clone());
                        self.set_status(message, StatusKind::Error);
                    } else {
                        self.refresh_song_screen()?;
                        self.refresh_song_manager()?;
                        self.set_status("Song updated.", StatusKind::Info);
                        keep_open = false;
                    }
                }
                Err(err) => {
                    let message = surface_error(&err);
                    form.error = Some(message.clone());
                    self.set_status(message, StatusKind::Error);
                }
            },
            KeyCode::Char(ch) => {
                if form.push_char(ch) {
                    form.error = None;
                    form.update_suggestion(&self.composers);
                }
            }
            _ => {}
        }

        if keep_open {
            Ok(Mode::EditingSong { song_id, form })
        } else {
            // If we stashed a search state before opening the modal, restore
            // it so the search remains active underneath the edit form.
            if let Some(state) = self.saved_search.take() {
                Ok(Mode::Searching(state))
            } else {
                Ok(Mode::Normal)
            }
        }
    }

    /// Confirmation dialog triggered when unlinking a song from the binder.
    fn handle_confirm_song_remove(
        &mut self,
        code: KeyCode,
        confirm: ConfirmSongRemove,
    ) -> Result<Mode> {
        match code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.set_status("Removal cancelled.", StatusKind::Info);
                Ok(Mode::Normal)
            }
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                match remove_song_from_binder(&self.conn, confirm.binder_id, confirm.song.id) {
                    Ok(_) => {
                        self.refresh_song_screen()?;
                        self.set_status("Song removed from binder.", StatusKind::Info);
                        Ok(Mode::Normal)
                    }
                    Err(err) => {
                        let message = surface_error(&err);
                        self.set_status(message, StatusKind::Error);
                        Ok(Mode::ConfirmSongRemove(confirm))
                    }
                }
            }
            _ => Ok(Mode::ConfirmSongRemove(confirm)),
        }
    }

    /// Confirmation dialog for permanently deleting a song from the database.
    fn handle_confirm_song_delete(
        &mut self,
        code: KeyCode,
        confirm: ConfirmSongDelete,
    ) -> Result<Mode> {
        match code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.set_status("Deletion cancelled.", StatusKind::Info);
                Ok(Mode::Normal)
            }
            KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                match delete_song(&self.conn, confirm.song.id) {
                    Ok(_) => {
                        self.refresh_song_manager()?;
                        self.refresh_song_screen()?;
                        self.set_status("Song deleted.", StatusKind::Info);
                        Ok(Mode::Normal)
                    }
                    Err(err) => {
                        let message = surface_error(&err);
                        self.set_status(message, StatusKind::Error);
                        Ok(Mode::ConfirmSongDelete(confirm))
                    }
                }
            }
            _ => Ok(Mode::ConfirmSongDelete(confirm)),
        }
    }

    /// Keyboard handler for the song selection palette. Supports navigation,
    /// search, and toggling without leaving the keyboard.
    fn handle_select_song(&mut self, code: KeyCode, mut state: AddSongState) -> Result<Mode> {
        match code {
            KeyCode::Esc => Ok(Mode::Normal),
            KeyCode::Up => {
                state.move_selection(-1);
                Ok(Mode::SelectingSong(state))
            }
            KeyCode::Down => {
                state.move_selection(1);
                Ok(Mode::SelectingSong(state))
            }
            KeyCode::PageUp => {
                state.move_selection(-5);
                Ok(Mode::SelectingSong(state))
            }
            KeyCode::PageDown => {
                state.move_selection(5);
                Ok(Mode::SelectingSong(state))
            }
            KeyCode::Home => {
                state.select_first();
                Ok(Mode::SelectingSong(state))
            }
            KeyCode::End => {
                state.select_last();
                Ok(Mode::SelectingSong(state))
            }
            KeyCode::Enter => match state.current_item() {
                Some(AddSongItem::CreateNew) => Ok(Mode::CreatingSong {
                    binder_id: Some(state.binder_id),
                    form: SongForm::default(),
                }),
                Some(AddSongItem::Existing(song)) => {
                    if let Err(err) = add_song_to_binder(&self.conn, state.binder_id, song.id) {
                        let message = surface_error(&err);
                        self.set_status(message, StatusKind::Error);
                        Ok(Mode::SelectingSong(state))
                    } else {
                        self.refresh_song_screen()?;
                        self.set_status("Song added to binder.", StatusKind::Info);
                        Ok(Mode::Normal)
                    }
                }
                None => Ok(Mode::Normal),
            },
            _ => Ok(Mode::SelectingSong(state)),
        }
    }

    /// Handle keys while an inline search is active. The search overlays the
    /// current song list and updates the filter as the user types. Esc clears
    /// the filter and exits the search, while navigation and Enter retain the
    /// normal song-list behavior against the filtered results.
    fn handle_search(&mut self, code: KeyCode, mut state: SearchState) -> Result<Mode> {
        match state.target {
            SearchTarget::SongManager => {
                // Ensure we're looking at the song manager; otherwise abort.
                let manager = match &mut self.screen {
                    Screen::SongManager(m) => m,
                    _ => return Ok(Mode::Normal),
                };

                match code {
                    KeyCode::Esc => {
                        manager.set_filter(None);
                        return Ok(Mode::Normal);
                    }
                    KeyCode::Up => {
                        manager.move_selection(-1);
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::Down => {
                        manager.move_selection(1);
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::PageUp => {
                        manager.move_selection(-5);
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::PageDown => {
                        manager.move_selection(5);
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::Home => {
                        manager.select_first();
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::End => {
                        manager.select_last();
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::Enter => {
                        if let Some(song) = manager.current_song().cloned() {
                            let link = song.link.trim().to_string();
                            if link.is_empty() {
                                self.set_status(
                                    "This song does not have a link.",
                                    StatusKind::Error,
                                );
                            } else if let Err(err) = open_link(&link) {
                                self.set_status(
                                    format!("Failed to open link: {err}"),
                                    StatusKind::Error,
                                );
                            } else {
                                self.set_status(
                                    format!("Opened {}.", song.display_title()),
                                    StatusKind::Info,
                                );
                            }
                        }
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::Backspace => {
                        state.query.pop();
                    }
                    KeyCode::Char(ch) => {
                        if ch.is_control() {
                            // Interpret Ctrl+E as Edit (0x05). Other controls are ignored
                            match ch {
                                '\u{5}' => {
                                    if let Some(song) = manager.current_song().cloned() {
                                        return Ok(Mode::EditingSong {
                                            song_id: song.id,
                                            form: SongForm::from_song(&song),
                                        });
                                    } else {
                                        self.set_status(
                                            "No song selected to edit.",
                                            StatusKind::Error,
                                        );
                                        return Ok(Mode::Searching(state));
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            state.query.push(ch);
                        }
                    }
                    _ => {}
                }

                if state.query.trim().is_empty() {
                    manager.set_filter(None);
                } else {
                    manager.set_filter(Some(state.query.clone()));
                }

                Ok(Mode::Searching(state))
            }
            SearchTarget::Songs => {
                let songs = match &mut self.screen {
                    Screen::Songs(s) => s,
                    _ => return Ok(Mode::Normal),
                };

                match code {
                    KeyCode::Esc => {
                        songs.set_filter(None);
                        return Ok(Mode::Normal);
                    }
                    KeyCode::Up => {
                        songs.move_selection(-1);
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::Down => {
                        songs.move_selection(1);
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::PageUp => {
                        songs.move_selection(-5);
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::PageDown => {
                        songs.move_selection(5);
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::Home => {
                        songs.select_first();
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::End => {
                        songs.select_last();
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::Enter => {
                        if let Some(song) = songs.current_song().cloned() {
                            let link = song.link.trim().to_string();
                            if link.is_empty() {
                                self.set_status(
                                    "This song does not have a link.",
                                    StatusKind::Error,
                                );
                            } else if let Err(err) = open_link(&link) {
                                self.set_status(
                                    format!("Failed to open link: {err}"),
                                    StatusKind::Error,
                                );
                            } else {
                                self.set_status(
                                    format!("Opened {}.", song.display_title()),
                                    StatusKind::Info,
                                );
                            }
                        }
                        return Ok(Mode::Searching(state));
                    }
                    KeyCode::Backspace => {
                        state.query.pop();
                    }
                    KeyCode::Char(ch) => {
                        if ch.is_control() {
                            // Ctrl+E -> Edit current song in binder view
                            match ch {
                                '\u{5}' => {
                                    if let Some(song) = songs.current_song().cloned() {
                                        return Ok(Mode::EditingSong {
                                            song_id: song.id,
                                            form: SongForm::from_song(&song),
                                        });
                                    } else {
                                        self.set_status(
                                            "No song selected to edit.",
                                            StatusKind::Error,
                                        );
                                        return Ok(Mode::Searching(state));
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            state.query.push(ch);
                        }
                    }
                    _ => {}
                }

                if state.query.trim().is_empty() {
                    songs.set_filter(None);
                } else {
                    songs.set_filter(Some(state.query.clone()));
                }

                Ok(Mode::Searching(state))
            }
        }
    }

    /// Draw a small search bar at the top of the provided `area` showing the
    /// current query and placing the cursor at the end of the typed text.
    fn draw_search_bar(&self, frame: &mut Frame, area: Rect, state: &SearchState) {
        let height = 3u16.min(area.height);
        let popup_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height,
        };
        frame.render_widget(Clear, popup_area);

        let block = Block::default().borders(Borders::ALL).title("Search");
        let paragraph = Paragraph::new(Span::raw(format!("Search: {}", state.query)))
            .block(block.clone())
            .wrap(Wrap { trim: true });
        frame.render_widget(paragraph, popup_area);

        let inner = block.inner(popup_area);
        let cursor_x = inner.x + "Search: ".len() as u16 + state.query.chars().count() as u16;
        let cursor_y = inner.y;
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    /// Create-song form handler that optionally links the song to a binder
    /// immediately after saving.
    fn handle_create_song(
        &mut self,
        code: KeyCode,
        binder_id: Option<i64>,
        mut form: SongForm,
    ) -> Result<Mode> {
        let mut keep_open = true;
        match code {
            KeyCode::Esc => {
                if !form.cancel_autocomplete() {
                    self.set_status("Song creation cancelled.", StatusKind::Info);
                    keep_open = false;
                }
            }
            KeyCode::Tab => {
                let consumed = form.has_active_suggestion() && form.accept_suggestion();
                if !consumed {
                    form.toggle_field();
                }
                form.update_suggestion(&self.composers);
            }
            KeyCode::BackTab => {
                form.toggle_field();
                form.update_suggestion(&self.composers);
            }
            KeyCode::Backspace => {
                form.backspace();
                form.update_suggestion(&self.composers);
            }
            KeyCode::Enter => match form.parse_inputs() {
                Ok((title, composer, link)) => {
                    match create_song(&self.conn, &title, &composer, &link) {
                        Ok(song) => {
                            if let Some(binder_id) = binder_id {
                                add_song_to_binder(&self.conn, binder_id, song.id)?;
                                self.refresh_song_screen()?;
                                self.set_status("Song created and added.", StatusKind::Info);
                            } else {
                                self.set_status("Song created.", StatusKind::Info);
                            }
                            self.refresh_song_manager()?;
                            keep_open = false;
                        }
                        Err(err) => {
                            let message = surface_error(&err);
                            form.error = Some(message.clone());
                            self.set_status(message, StatusKind::Error);
                        }
                    }
                }
                Err(err) => {
                    let message = surface_error(&err);
                    form.error = Some(message.clone());
                    self.set_status(message, StatusKind::Error);
                }
            },
            KeyCode::Char(ch) => {
                if form.push_char(ch) {
                    form.error = None;
                    form.update_suggestion(&self.composers);
                }
            }
            _ => {}
        }

        if keep_open {
            Ok(Mode::CreatingSong { binder_id, form })
        } else {
            Ok(Mode::Normal)
        }
    }

    /// Unsaved-changes confirmation handler for the "To Print" view. It pairs
    /// with `apply_to_print_changes` to ensure the user does not lose work.
    fn handle_confirm_to_print_exit(
        &mut self,
        code: KeyCode,
        mut confirm: ConfirmToPrintExit,
        exit: &mut bool,
    ) -> Result<Mode> {
        match code {
            KeyCode::Esc => Ok(Mode::Normal),
            KeyCode::Left | KeyCode::Up => {
                confirm.previous();
                Ok(Mode::ConfirmToPrintExit(confirm))
            }
            KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
                confirm.next();
                Ok(Mode::ConfirmToPrintExit(confirm))
            }
            KeyCode::Enter => match confirm.selection {
                ConfirmPrintChoice::Apply => {
                    let assignments = if let Screen::ToPrint(report) = &self.screen {
                        report.pending_assignments()
                    } else {
                        Vec::new()
                    };

                    match self.apply_to_print_changes(&assignments) {
                        Ok(applied) => {
                            let message = if applied == 0 {
                                "No changes to apply.".to_string()
                            } else {
                                let plural = if applied == 1 { "" } else { "s" };
                                format!("Applied {applied} song{plural}.")
                            };
                            self.set_status(message, StatusKind::Info);
                        }
                        Err(err) => {
                            let message = surface_error(&err);
                            self.set_status(message, StatusKind::Error);
                            return Ok(Mode::ConfirmToPrintExit(confirm));
                        }
                    }

                    if confirm.exit_app {
                        *exit = true;
                    } else {
                        self.screen = Screen::Binders;
                    }
                    Ok(Mode::Normal)
                }
                ConfirmPrintChoice::Discard => {
                    if confirm.exit_app {
                        *exit = true;
                    } else {
                        self.set_status("Discarded pending changes.", StatusKind::Info);
                        self.screen = Screen::Binders;
                    }
                    Ok(Mode::Normal)
                }
                ConfirmPrintChoice::Cancel => Ok(Mode::Normal),
            },
            _ => Ok(Mode::ConfirmToPrintExit(confirm)),
        }
    }

    /// Persist a new binder using the data gathered in the form and refresh the
    /// local binder cache. The helper centralizes success messaging so calling
    /// sites stay lean.
    fn save_new_binder(&mut self, form: &BinderForm) -> Result<()> {
        let (number, label) = form.parse_inputs()?;
        let binder = create_binder(&self.conn, number, &label)?;
        self.reload_binders(Some(binder.id))?;
        self.set_status(
            format!("Added Binder {:02}.", binder.number),
            StatusKind::Info,
        );
        Ok(())
    }

    /// Update a binder and refresh both the cached list and any open binder
    /// detail view so the UI reflects the new label/number immediately.
    fn save_existing_binder(&mut self, id: i64, form: &BinderForm) -> Result<()> {
        let (number, label) = form.parse_inputs()?;
        update_binder(&self.conn, id, number, &label)?;
        self.reload_binders(Some(id))?;
        self.set_status(format!("Updated Binder {:02}.", number), StatusKind::Info);
        if let Screen::Songs(ref mut songs) = self.screen {
            if songs.binder.id == id {
                songs.binder.label = label;
                songs.binder.number = number;
            }
        }
        Ok(())
    }

    /// Delete the binder confirmed by the user, then reset to the grid view.
    fn perform_delete(&mut self, confirm: &ConfirmBinderDelete) -> Result<()> {
        delete_binder(&self.conn, confirm.id)?;
        self.reload_binders(None)?;
        self.screen = Screen::Binders;
        self.set_status(
            format!("Deleted Binder {:02}.", confirm.number),
            StatusKind::Info,
        );
        Ok(())
    }

    /// Reload binders from the database and optionally focus a specific id. The
    /// focus logic lets us keep the user's place after updates.
    fn reload_binders(&mut self, focus_id: Option<i64>) -> Result<()> {
        self.binders = fetch_binders(&self.conn)?;
        if self.binders.is_empty() {
            self.selected = 0;
            return Ok(());
        }

        if let Some(id) = focus_id {
            if let Some((idx, _)) = self.binders.iter().enumerate().find(|(_, b)| b.id == id) {
                self.selected = idx;
                return Ok(());
            }
        }

        if self.selected >= self.binders.len() {
            self.selected = self.binders.len().saturating_sub(1);
        }

        Ok(())
    }

    /// Transition into the binder detail screen by loading its songs.
    fn open_binder_view(&mut self, binder: Binder) -> Result<()> {
        let songs = fetch_songs_for_binder(&self.conn, binder.id)?;
        self.screen = Screen::Songs(SongScreen::new(binder, songs));
        Ok(())
    }

    /// With a detail view open, jump to the next or previous binder based on
    /// numeric order. The modulo arithmetic keeps the navigation circular.
    fn open_relative_binder(&mut self, offset: isize) -> Result<()> {
        if self.binders.is_empty() {
            return Ok(());
        }

        let current_id = match &self.screen {
            Screen::Songs(songs) => songs.binder.id,
            _ => return Ok(()),
        };

        let (target_id, binder_clone) = {
            let mut ordered: Vec<&Binder> = self.binders.iter().collect();
            ordered.sort_by_key(|binder| binder.number);
            if ordered.is_empty() {
                return Ok(());
            }

            let len = ordered.len() as isize;
            let current_pos = ordered
                .iter()
                .position(|binder| binder.id == current_id)
                .unwrap_or(0);
            let new_pos = ((current_pos as isize + offset).rem_euclid(len)) as usize;
            let binder_ref = ordered[new_pos];
            (binder_ref.id, binder_ref.clone())
        };

        if let Some(idx) = self
            .binders
            .iter()
            .position(|binder| binder.id == target_id)
        {
            self.selected = idx;
        }

        self.open_binder_view(binder_clone)
    }

    /// Load all songs and open the song manager screen. We also refresh
    /// composers so autocomplete suggestions stay current.
    fn open_song_manager(&mut self) -> Result<()> {
        let songs = fetch_all_songs(&self.conn)?;
        self.reload_composers()?;
        self.screen = Screen::SongManager(SongManagerScreen::new(songs));
        Ok(())
    }

    /// Build the "To Print" report, ensuring the director binder exists before
    /// constructing per-binder summaries.
    fn open_to_print_view(&mut self) -> Result<()> {
        if let Some(director) = self
            .binders
            .iter()
            .find(|binder| binder.number == 0)
            .cloned()
        {
            let director_songs = fetch_songs_for_binder(&self.conn, director.id)?;
            let mut binder_reports = Vec::new();
            let mut song_totals: Vec<SongNeeded> = Vec::new();

            for binder in self
                .binders
                .iter()
                .filter(|binder| binder.id != director.id)
            {
                let songs = fetch_songs_for_binder(&self.conn, binder.id)?;
                let song_ids: HashSet<i64> = songs.iter().map(|song| song.id).collect();

                let mut missing = Vec::new();
                for song in &director_songs {
                    if !song_ids.contains(&song.id) {
                        missing.push(MissingSong {
                            song: song.clone(),
                            checked: false,
                        });

                        if let Some(entry) = song_totals
                            .iter_mut()
                            .find(|entry| entry.song.id == song.id)
                        {
                            entry.needed += 1;
                        } else {
                            song_totals.push(SongNeeded {
                                song: song.clone(),
                                needed: 1,
                            });
                        }
                    }
                }

                if !missing.is_empty() {
                    binder_reports.push(BinderReport {
                        binder_id: binder.id,
                        binder_number: binder.number,
                        binder_label: binder.label.clone(),
                        songs: missing,
                    });
                }
            }

            self.screen = Screen::ToPrint(ToPrintScreen::with_data(binder_reports, song_totals));
        } else {
            self.screen = Screen::ToPrint(ToPrintScreen::missing_director());
        }

        Ok(())
    }

    /// Apply the pending assignments from the "To Print" flow by creating the
    /// binder-song links. Returns the number of associations created so we can
    /// craft meaningful status messages.
    fn apply_to_print_changes(&mut self, assignments: &[(i64, i64)]) -> Result<usize> {
        let mut applied = 0;
        for &(binder_id, song_id) in assignments {
            add_song_to_binder(&self.conn, binder_id, song_id)?;
            applied += 1;
        }

        if applied > 0 {
            self.refresh_song_manager()?;
            self.refresh_song_screen()?;
        }

        Ok(applied)
    }

    /// Reload the songs for the currently open binder detail view, if any.
    fn refresh_song_screen(&mut self) -> Result<()> {
        if let Screen::Songs(ref mut songs) = self.screen {
            let updated = fetch_songs_for_binder(&self.conn, songs.binder.id)?;
            songs.set_songs(updated);
        }
        Ok(())
    }

    /// Refresh the master song list and autocomplete cache.
    fn refresh_song_manager(&mut self) -> Result<()> {
        if let Screen::SongManager(ref mut manager) = self.screen {
            let updated = fetch_all_songs(&self.conn)?;
            manager.set_songs(updated);
        }
        self.reload_composers()?;
        Ok(())
    }

    /// Refresh the cached composer list used for autocomplete suggestions.
    fn reload_composers(&mut self) -> Result<()> {
        self.composers = fetch_composers(&self.conn)?;
        Ok(())
    }

    /// Predict the next binder number by scanning the loaded binders.
    fn next_binder_number(&self) -> i64 {
        self.binders
            .iter()
            .map(|binder| binder.number)
            .max()
            .unwrap_or(0)
            + 1
    }

    /// Return the currently highlighted binder, if any.
    fn current_binder(&self) -> Option<&Binder> {
        self.binders.get(self.selected)
    }

    /// Cached binder count, exposed for readability.
    fn binder_count(&self) -> usize {
        self.binders.len()
    }

    /// Number of rows needed for the grid given the binder count and column
    /// layout.
    fn row_count(&self) -> usize {
        let cols = GRID_COLUMNS.max(1);
        (self.binder_count() + cols - 1) / cols
    }

    /// Move the grid selection left or right by one cell, guarding against
    /// wrapping so keyboard navigation feels predictable.
    fn move_horizontal(&mut self, offset: isize) {
        if matches!(self.screen, Screen::Binders) && !self.binders.is_empty() {
            let new_index = self.selected as isize + offset;
            if (0..self.binder_count() as isize).contains(&new_index) {
                self.selected = new_index as usize;
            }
        }
    }

    /// Move the grid selection up or down by one row.
    fn move_vertical(&mut self, offset: isize) {
        if matches!(self.screen, Screen::Binders) && !self.binders.is_empty() {
            let cols = GRID_COLUMNS as isize;
            let new_index = self.selected as isize + offset * cols;
            if (0..self.binder_count() as isize).contains(&new_index) {
                self.selected = new_index as usize;
            }
        }
    }

    /// Main render routine invoked each tick by Ratatui. Splits the frame into
    /// content and footer regions and dispatches to the active screen.
    fn draw(&self, frame: &mut Frame) {
        let area = frame.area();
        let footer_height = FOOTER_HEIGHT.min(area.height);

        let (content_area, footer_area) = if area.height > footer_height {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(footer_height)])
                .split(area);
            (chunks[0], chunks[1])
        } else {
            (area, area)
        };

        match &self.screen {
            Screen::Binders => self.draw_binder_grid(frame, content_area),
            Screen::Songs(songs) => self.draw_song_view(frame, content_area, songs),
            Screen::SongManager(manager) => self.draw_song_manager(frame, content_area, manager),
            Screen::ToPrint(report) => self.draw_to_print(frame, content_area, report),
        }

        if area.height >= footer_height {
            self.draw_footer(frame, footer_area);
        }

        match &self.mode {
            Mode::AddingBinder(form) => self.draw_binder_form(frame, area, "Add Binder", form),
            Mode::EditingBinder { form, .. } => {
                self.draw_binder_form(frame, area, "Edit Binder", form)
            }
            Mode::ConfirmBinderDelete(confirm) => self.draw_confirm_binder(frame, area, confirm),
            Mode::EditingSong { form, .. } => self.draw_song_form(frame, area, "Edit Song", form),
            Mode::ConfirmSongRemove(confirm) => self.draw_confirm_song(frame, area, confirm),
            Mode::ConfirmSongDelete(confirm) => self.draw_confirm_song_delete(frame, area, confirm),
            Mode::SelectingSong(state) => self.draw_add_song(frame, area, state),
            Mode::CreatingSong { form, .. } => {
                self.draw_song_form(frame, area, "Create Song", form)
            }
            Mode::ConfirmToPrintExit(confirm) => {
                self.draw_confirm_to_print_exit(frame, area, confirm)
            }
            Mode::Searching(state) => self.draw_search_bar(frame, area, state),
            Mode::Normal => {}
        }
    }

    /// Called from the event loop when Ctrl+E is pressed so we can escape
    /// the search box and open the edit form for the current song.
    fn handle_ctrl_e(&mut self) -> Result<()> {
        // Only act when the search overlay is active.
        if !matches!(self.mode, Mode::Searching(_)) {
            return Ok(());
        }

        // Extract and stash the active SearchState so it can be restored after
        // the edit modal closes.
        let previous = mem::replace(&mut self.mode, Mode::Normal);
        if let Mode::Searching(state) = previous {
            self.saved_search = Some(state);
        }

        // Open the edit modal for the currently-selected song, if any.
        match &mut self.screen {
            Screen::SongManager(manager) => {
                if let Some(song) = manager.current_song().cloned() {
                    self.mode = Mode::EditingSong {
                        song_id: song.id,
                        form: SongForm::from_song(&song),
                    };
                } else {
                    self.set_status("No song selected to edit.", StatusKind::Error);
                }
            }
            Screen::Songs(songs) => {
                if let Some(song) = songs.current_song().cloned() {
                    self.mode = Mode::EditingSong {
                        song_id: song.id,
                        form: SongForm::from_song(&song),
                    };
                } else {
                    self.set_status("No song selected to edit.", StatusKind::Error);
                }
            }
            _ => {}
        }

        Ok(())
    }

    /// Toggle the "show only songs without links" filter in the song manager,
    /// preserving any active search query.
    fn handle_ctrl_l(&mut self) -> Result<()> {
        if let Screen::SongManager(manager) = &mut self.screen {
            let active = manager.toggle_show_no_link();
            let message = if active {
                "Showing songs without links.".to_string()
            } else {
                "Showing all songs.".to_string()
            };
            self.set_status(message, StatusKind::Info);
        }
        Ok(())
    }

    /// Render the binder overview grid with decorative covers and selection
    /// highlighting.
    fn draw_binder_grid(&self, frame: &mut Frame, area: Rect) {
        if self.binders.is_empty() {
            let message = Paragraph::new("No binders yet. Press '+' to add one.")
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::NONE));
            frame.render_widget(message, area);
            return;
        }

        let rows = self.split_rows(area);
        for (row_idx, row_chunk) in rows.into_iter().enumerate() {
            let columns = self.split_columns(row_chunk);
            for (col_idx, column_chunk) in columns.into_iter().enumerate() {
                let binder_index = row_idx * GRID_COLUMNS + col_idx;
                if let Some(binder) = self.binders.get(binder_index) {
                    let mut block = Block::default()
                        .borders(Borders::ALL)
                        .title(format!("Binder {:02}", binder.number));
                    if binder_index == self.selected {
                        block = block.style(Style::default().fg(Color::Yellow));
                    }
                    let pattern = BINDER_ART[binder_index % BINDER_ART.len()];
                    let inner_width = column_chunk.width.saturating_sub(2);
                    let inner_height = column_chunk.height.saturating_sub(2);
                    let lines = build_binder_cover_lines(
                        binder,
                        pattern,
                        inner_width,
                        inner_height,
                        binder_index == self.selected,
                    );
                    let card = Paragraph::new(lines)
                        .alignment(Alignment::Left)
                        .block(block);
                    frame.render_widget(card, column_chunk);
                }
            }
        }
    }

    /// Render the songs attached to a specific binder, including metadata in
    /// the header.
    fn draw_song_view(&self, frame: &mut Frame, area: Rect, songs: &SongScreen) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(1)])
            .split(area);

        let header = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    format!("Binder {:02}", songs.binder.number),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("  •  {}", songs.binder.label)),
            ]),
            Line::from(Span::raw(format!("{} songs linked", songs.songs.len()))),
        ])
        .alignment(Alignment::Left)
        .block(Block::default().borders(Borders::ALL).title("Binder Songs"));
        frame.render_widget(header, chunks[0]);

        if songs.songs.is_empty() {
            let message = Paragraph::new("No songs yet. Press '+' to add one.")
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(message, chunks[1]);
            return;
        }

        self.render_song_cards(frame, chunks[1], &songs.filtered_songs, songs.selected);
    }

    /// Render the global song manager list when accessed from the home screen.
    fn draw_song_manager(&self, frame: &mut Frame, area: Rect, manager: &SongManagerScreen) {
        let mut list_area = area;

        if manager.show_only_no_link {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(4), Constraint::Min(1)])
                .split(area);
            let indicator = Paragraph::new(vec![
                Line::from(Span::styled(
                    "Song Manager",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(vec![
                    Span::styled(
                        "No-link filter active",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" - showing only songs without links (press "),
                    Span::styled(
                        "[l]",
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" to show all)"),
                ]),
            ])
            .block(Block::default().borders(Borders::ALL))
            .alignment(Alignment::Left);
            frame.render_widget(indicator, chunks[0]);
            list_area = chunks[1];
        }

        if list_area.height == 0 {
            return;
        }

        if manager.songs.is_empty() {
            let message = Paragraph::new("No songs yet. Press '+' to add one.")
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title("All Songs"));
            frame.render_widget(message, list_area);
            return;
        }

        if manager.filtered_songs.is_empty() {
            let has_search = manager
                .filter
                .as_ref()
                .map(|q| !q.trim().is_empty())
                .unwrap_or(false);
            let message_text = if manager.show_only_no_link && has_search {
                "No songs match the current search without links."
            } else if manager.show_only_no_link {
                "No songs without links yet."
            } else if has_search {
                "No songs match the current search."
            } else {
                "No songs to display."
            };
            let message = Paragraph::new(message_text)
                .alignment(Alignment::Center)
                .block(Block::default().borders(Borders::ALL).title("All Songs"));
            frame.render_widget(message, list_area);
            return;
        }

        self.render_song_cards(frame, list_area, &manager.filtered_songs, manager.selected);
    }

    /// Render the printable report view, showing either binder-by-binder needs
    /// or aggregate song totals based on the active mode.
    fn draw_to_print(&self, frame: &mut Frame, area: Rect, report: &ToPrintScreen) {
        let title = match report.mode {
            ToPrintMode::ByBinder => "To Print • By Binder",
            ToPrintMode::BySong => "To Print • By Song",
        };
        let block = Block::default().title(title).borders(Borders::ALL);

        if !report.director_exists {
            let paragraph = Paragraph::new("Director's binder missing")
                .alignment(Alignment::Center)
                .block(block);
            frame.render_widget(paragraph, area);
            return;
        }

        let lines = report.display_lines();
        let content = if lines.is_empty() {
            String::from("Nothing to print.")
        } else {
            lines.join("\n")
        };

        let paragraph = Paragraph::new(content)
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((report.scroll, 0));
        frame.render_widget(paragraph, area);
    }

    /// Render the footer that hosts transient status messages and the current
    /// set of keyboard shortcuts.
    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default().borders(Borders::TOP);
        frame.render_widget(block.clone(), area);
        let inner = block.inner(area);

        let status_line = if let Some(status) = &self.status {
            Line::from(vec![Span::styled(status.text.clone(), status.kind.style())])
        } else {
            Line::from("")
        };

        let instructions = self.footer_instructions();

        let paragraph = Paragraph::new(vec![status_line, instructions]).wrap(Wrap { trim: true });
        frame.render_widget(paragraph, inner);
    }

    /// Build the instruction line based on the active screen/mode. Keeping this
    /// logic centralized avoids duplication inside `draw_footer`.
    fn footer_instructions(&self) -> Line<'static> {
        let key_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        match (&self.screen, &self.mode) {
            (_, Mode::SelectingSong(_)) => Line::from(vec![
                Span::styled("[↑↓]", key_style),
                Span::raw(" Navigate   "),
                Span::styled("[Enter]", key_style),
                Span::raw(" Choose   "),
                Span::styled("[Esc]", key_style),
                Span::raw(" Cancel"),
            ]),
            (Screen::ToPrint(report), _) => {
                if report.director_exists {
                    Line::from(vec![
                        Span::styled("[Space]", key_style),
                        Span::raw(" Toggle   "),
                        Span::styled("[Tab]", key_style),
                        Span::raw(" Toggle View   "),
                        Span::styled("[↑↓]", key_style),
                        Span::raw(" Navigate   "),
                        Span::styled("[PgUp/PgDn]", key_style),
                        Span::raw(" Page   "),
                        Span::styled("[p]", key_style),
                        Span::raw(" Back   "),
                        Span::styled("[q]", key_style),
                        Span::raw(" Quit"),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled("[p]", key_style),
                        Span::raw(" Back   "),
                        Span::styled("[q]", key_style),
                        Span::raw(" Quit"),
                    ])
                }
            }
            (Screen::SongManager(_), _) => Line::from(vec![
                Span::styled("[↑↓]", key_style),
                Span::raw(" Select   "),
                Span::styled("[Enter]", key_style),
                Span::raw(" Open Link   "),
                Span::styled("[f]", key_style),
                Span::raw(" Search   "),
                Span::styled("[l]", key_style),
                Span::raw(" Toggle No-Link   "),
                Span::styled("[+]", key_style),
                Span::raw(" Add   "),
                Span::styled("[-]", key_style),
                Span::raw(" Delete   "),
                Span::styled("[e]", key_style),
                Span::raw(" Edit   "),
                Span::styled("[p]", key_style),
                Span::raw(" To Print   "),
                Span::styled("[s]", key_style),
                Span::raw(" Binders   "),
                Span::styled("[q]", key_style),
                Span::raw(" Quit"),
            ]),
            (Screen::Songs(_), _) => Line::from(vec![
                Span::styled("[↑↓]", key_style),
                Span::raw(" Select   "),
                Span::styled("[Enter]", key_style),
                Span::raw(" Open Link   "),
                Span::styled("[f]", key_style),
                Span::raw(" Search   "),
                Span::styled("[+]", key_style),
                Span::raw(" Add   "),
                Span::styled("[-]", key_style),
                Span::raw(" Remove   "),
                Span::styled("[e]", key_style),
                Span::raw(" Edit   "),
                Span::styled("[s]", key_style),
                Span::raw(" Song Manager   "),
                Span::styled("[p]", key_style),
                Span::raw(" To Print   "),
                Span::styled("[Esc]", key_style),
                Span::raw(" Back   "),
                Span::styled("[q]", key_style),
                Span::raw(" Quit"),
            ]),
            _ => Line::from(vec![
                Span::styled("[←↑↓→]", key_style),
                Span::raw(" Move   "),
                Span::styled("[Enter]", key_style),
                Span::raw(" Open   "),
                Span::styled("[+]", key_style),
                Span::raw(" Add   "),
                Span::styled("[-]", key_style),
                Span::raw(" Remove   "),
                Span::styled("[e]", key_style),
                Span::raw(" Edit   "),
                Span::styled("[s]", key_style),
                Span::raw(" Song Manager   "),
                Span::styled("[p]", key_style),
                Span::raw(" To Print   "),
                Span::styled("[q]", key_style),
                Span::raw(" Quit"),
            ]),
        }
    }

    /// Render the add/edit binder dialog. The layout centers the form and
    /// surfaces validation feedback below the fields.
    fn draw_binder_form(&self, frame: &mut Frame, area: Rect, title: &str, form: &BinderForm) {
        let popup_area = centered_rect(60, 40, area);
        frame.render_widget(Clear, popup_area);

        let block = Block::default().title(title).borders(Borders::ALL);
        frame.render_widget(block.clone(), popup_area);
        let inner = block.inner(popup_area);

        let number_line = form.build_line("Number", BinderField::Number);
        let label_line = form.build_line("Label", BinderField::Label);

        let mut lines = vec![number_line, label_line, Line::from("")];

        if let Some(error) = &form.error {
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::default().fg(Color::Red),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "Enter to save • Tab to accept/switch • Esc to cancel",
                Style::default().fg(Color::Gray),
            )));
        }

        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
        frame.render_widget(paragraph, inner);

        let (cursor_x, cursor_y) = match form.active {
            BinderField::Number => {
                let prefix = "Number: ".len() as u16;
                (
                    inner.x + prefix + form.value_len(BinderField::Number) as u16,
                    inner.y,
                )
            }
            BinderField::Label => {
                let prefix = "Label: ".len() as u16;
                (
                    inner.x + prefix + form.value_len(BinderField::Label) as u16,
                    inner.y + 1,
                )
            }
        };
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    /// Render the add/edit song dialog, including autocomplete hints for the
    /// composer field.
    fn draw_song_form(&self, frame: &mut Frame, area: Rect, title: &str, form: &SongForm) {
        let popup_area = centered_rect(70, 50, area);
        frame.render_widget(Clear, popup_area);

        let block = Block::default().title(title).borders(Borders::ALL);
        frame.render_widget(block.clone(), popup_area);
        let inner = block.inner(popup_area);

        let title_line = form.build_line("Title", SongField::Title);
        let composer_line = form.build_line("Composer", SongField::Composer);
        let link_line = form.build_line("Link", SongField::Link);

        let mut lines = vec![title_line, composer_line, link_line, Line::from("")];

        if let Some(error) = &form.error {
            lines.push(Line::from(Span::styled(
                error.clone(),
                Style::default().fg(Color::Red),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "Enter to save • Tab to switch • Esc to cancel",
                Style::default().fg(Color::Gray),
            )));
        }

        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
        frame.render_widget(paragraph, inner);

        let (cursor_x, cursor_y) = match form.active {
            SongField::Title => {
                let prefix = "Title: ".len() as u16;
                (
                    inner.x + prefix + form.value_len(SongField::Title) as u16,
                    inner.y,
                )
            }
            SongField::Composer => {
                let prefix = "Composer: ".len() as u16;
                (
                    inner.x + prefix + form.value_len(SongField::Composer) as u16,
                    inner.y + 1,
                )
            }
            SongField::Link => {
                let prefix = "Link: ".len() as u16;
                (
                    inner.x + prefix + form.value_len(SongField::Link) as u16,
                    inner.y + 2,
                )
            }
        };
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    /// Render the binder deletion confirmation popup.
    fn draw_confirm_binder(&self, frame: &mut Frame, area: Rect, confirm: &ConfirmBinderDelete) {
        let popup_area = centered_rect(60, 30, area);
        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title("Confirm Removal")
            .borders(Borders::ALL);
        frame.render_widget(block.clone(), popup_area);
        let inner = block.inner(popup_area);

        let lines = vec![
            Line::from(format!(
                "Remove Binder {:02} ({})?",
                confirm.number, confirm.label
            )),
            Line::from("This will also remove any linked songs."),
            Line::from(""),
            Line::from(Span::styled(
                "Press Y to confirm or N / Esc to cancel.",
                Style::default().fg(Color::Gray),
            )),
        ];

        let paragraph = Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true });
        frame.render_widget(paragraph, inner);
    }

    /// Render the confirmation dialog for unlinking a song from a binder.
    fn draw_confirm_song(&self, frame: &mut Frame, area: Rect, confirm: &ConfirmSongRemove) {
        let popup_area = centered_rect(60, 30, area);
        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title("Remove Song from Binder")
            .borders(Borders::ALL);
        frame.render_widget(block.clone(), popup_area);
        let inner = block.inner(popup_area);

        let lines = vec![
            Line::from(format!(
                "Remove '{}' from this binder?",
                confirm.song.display_title()
            )),
            Line::from("This will not delete the song from other binders."),
            Line::from(""),
            Line::from(Span::styled(
                "Press Y to confirm or N / Esc to cancel.",
                Style::default().fg(Color::Gray),
            )),
        ];

        let paragraph = Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true });
        frame.render_widget(paragraph, inner);
    }

    /// Render the confirmation dialog for deleting a song entirely.
    fn draw_confirm_song_delete(&self, frame: &mut Frame, area: Rect, confirm: &ConfirmSongDelete) {
        let popup_area = centered_rect(60, 30, area);
        frame.render_widget(Clear, popup_area);

        let block = Block::default().title("Delete Song").borders(Borders::ALL);
        frame.render_widget(block.clone(), popup_area);
        let inner = block.inner(popup_area);

        let lines = vec![
            Line::from(format!(
                "Delete '{}' permanently?",
                confirm.song.display_title()
            )),
            Line::from("This will remove the song from all binders."),
            Line::from(""),
            Line::from(Span::styled(
                "Press Y to confirm or N / Esc to cancel.",
                Style::default().fg(Color::Gray),
            )),
        ];

        let paragraph = Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: true });
        frame.render_widget(paragraph, inner);
    }

    /// Render the unsaved-changes dialog for exiting the "To Print" report.
    fn draw_confirm_to_print_exit(
        &self,
        frame: &mut Frame,
        area: Rect,
        confirm: &ConfirmToPrintExit,
    ) {
        let popup_area = centered_rect(70, 40, area);
        frame.render_widget(Clear, popup_area);

        let title = if confirm.exit_app {
            "Exit Application"
        } else {
            "Leave To Print"
        };
        let block = Block::default().title(title).borders(Borders::ALL);
        frame.render_widget(block.clone(), popup_area);
        let inner = block.inner(popup_area);

        let message = if confirm.exit_app {
            "You have marked songs as added. Apply the changes before quitting?"
        } else {
            "You have marked songs as added. Apply the changes before leaving?"
        };

        let mut option_spans = Vec::new();
        for (idx, label) in confirm.labels().iter().enumerate() {
            if idx > 0 {
                option_spans.push(Span::raw("   "));
            }
            let style = if confirm.selected_index() == idx {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            option_spans.push(Span::styled(*label, style));
        }

        let lines = vec![
            Line::from(message),
            Line::from(""),
            Line::from(option_spans),
            Line::from(""),
            Line::from(Span::styled(
                "Use ←/→ to choose • Enter to confirm • Esc to cancel",
                Style::default().fg(Color::Gray),
            )),
        ];

        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: true });
        frame.render_widget(paragraph, inner);
    }

    /// Helper shared by song-related screens to display the scrollable list of
    /// songs.
    fn render_song_cards(&self, frame: &mut Frame, area: Rect, songs: &[Song], selected: usize) {
        if songs.is_empty() || area.height == 0 {
            return;
        }

        let card_height = SONG_CARD_HEIGHT as usize;
        let capacity = ((area.height as usize) / card_height).max(1);
        let len = songs.len();
        let mut start = if selected >= capacity {
            selected + 1 - capacity
        } else {
            0
        };
        if start + capacity > len {
            start = len.saturating_sub(capacity);
        }
        let end = min(start + capacity, len);
        let visible_len = end.saturating_sub(start);
        if visible_len == 0 {
            return;
        }

        let constraints: Vec<Constraint> = (0..visible_len)
            .map(|_| Constraint::Length(SONG_CARD_HEIGHT))
            .collect();
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        for (idx, chunk) in rows.iter().enumerate() {
            if chunk.height == 0 {
                continue;
            }

            let song_index = start + idx;
            if song_index >= len {
                break;
            }

            let song = &songs[song_index];
            let mut block = Block::default().borders(Borders::ALL);
            let mut paragraph_style = Style::default();
            if song_index == selected {
                block = block.style(Style::default().fg(Color::Yellow));
                paragraph_style = Style::default().fg(Color::Yellow);
            }

            let mut lines = Vec::new();
            let title = if song_index == selected {
                format!("▶ {}", song.title)
            } else {
                song.title.clone()
            };
            lines.push(Line::from(Span::styled(
                title,
                Style::default().add_modifier(Modifier::BOLD),
            )));

            let composer_text = if song.composer.trim().is_empty() {
                "Unknown composer".to_string()
            } else {
                song.composer.trim().to_string()
            };
            lines.push(Line::from(Span::styled(
                composer_text,
                Style::default().fg(Color::Gray),
            )));

            if !song.link.trim().is_empty() {
                lines.push(Line::from(Span::styled(
                    song.link.trim().to_string(),
                    Style::default().fg(Color::Cyan),
                )));
            }

            let paragraph = Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: true })
                .alignment(Alignment::Left)
                .style(paragraph_style);

            frame.render_widget(paragraph, *chunk);
        }
    }

    /// Render the song selection palette used when attaching songs to a binder.
    fn draw_add_song(&self, frame: &mut Frame, area: Rect, state: &AddSongState) {
        let popup_area = centered_rect(70, 50, area);
        frame.render_widget(Clear, popup_area);

        let block = Block::default()
            .title("Add Song to Binder")
            .borders(Borders::ALL);
        frame.render_widget(block.clone(), popup_area);
        let inner = block.inner(popup_area);

        let items: Vec<ListItem> = state
            .items
            .iter()
            .map(|item| match item {
                AddSongItem::CreateNew => ListItem::new("Create a new song"),
                AddSongItem::Existing(song) => ListItem::new(song.display_title()),
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::NONE))
            .highlight_style(Style::default().fg(Color::Yellow))
            .highlight_symbol("▶ ");

        let mut list_state = ListState::default();
        list_state.select(Some(state.selected));
        frame.render_stateful_widget(list, inner, &mut list_state);
    }

    /// Split the main area into evenly sized rows based on the binder count.
    fn split_rows(&self, area: Rect) -> Vec<Rect> {
        let row_count = self.row_count().max(1) as u16;
        let percent = (100 / row_count).max(1);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![Constraint::Percentage(percent); row_count as usize])
            .split(area);
        chunks.iter().cloned().collect()
    }

    /// Split a row into evenly sized columns. `GRID_COLUMNS` drives the count.
    fn split_columns(&self, area: Rect) -> Vec<Rect> {
        let columns = GRID_COLUMNS.max(1) as u16;
        let percent = (100 / columns).max(1);
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Percentage(percent); columns as usize])
            .split(area);
        chunks.iter().cloned().collect()
    }

    /// Set a status message that will appear in the footer on the next draw
    /// call.
    fn set_status<S: Into<String>>(&mut self, text: S, kind: StatusKind) {
        self.status = Some(StatusMessage {
            text: text.into(),
            kind,
        });
    }

    /// Clear any existing status from the footer.
    fn clear_status(&mut self) {
        self.status = None;
    }
}
/// Spin up the terminal backend, enter the draw loop, and keep processing input
/// until the user quits. Errors bubble up to the caller so the binary can
/// render an informative message before exiting.
pub fn run_app(app: &mut App) -> Result<()> {
    let mut stdout = io::stdout();
    enable_raw_mode().context("failed to enable raw mode")?;
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal backend")?;

    let result = loop {
        terminal
            .draw(|frame| app.draw(frame))
            .context("failed to draw frame")?;

        if event::poll(Duration::from_millis(250)).context("event polling failed")? {
            if let Event::Key(key_event) = event::read().context("failed to read event")? {
                if key_event.kind == KeyEventKind::Press {
                    // Intercept Ctrl+E while searching and route to a dedicated
                    // handler so the control is not treated as a printable char.
                    if key_event.modifiers.contains(KeyModifiers::CONTROL) {
                        match key_event.code {
                            KeyCode::Char('e') => {
                                app.handle_ctrl_e()?;
                                continue;
                            }
                            KeyCode::Char('l') => {
                                app.handle_ctrl_l()?;
                                continue;
                            }
                            _ => {}
                        }
                    }

                    if app.handle_key(key_event.code)? {
                        break Ok(());
                    }
                }
            }
        }
    };

    cleanup_terminal(&mut terminal)?;
    result
}
/// Restore the terminal to its original state after the app exits.
fn cleanup_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().context("failed to disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)
        .context("failed to leave alternate screen")?;
    terminal
        .show_cursor()
        .context("failed to restore cursor visibility")
}

/// Internal representation of the "binder" form fields. Keeping the state
/// separate from `App` lets us stash validation errors and cursor position.
#[derive(Default, Clone)]
struct BinderForm {
    number: String,
    label: String,
    active: BinderField,
    error: Option<String>,
}

/// Fields available within the binder form. Used to determine which input is
/// currently active.
#[derive(Copy, Clone, PartialEq, Eq)]
enum BinderField {
    Number,
    Label,
}

impl Default for BinderField {
    fn default() -> Self {
        BinderField::Number
    }
}

impl BinderForm {
    /// Seed the form with the suggested next binder number.
    fn with_number(number: i64) -> Self {
        let mut form = Self::default();
        if number > 0 {
            form.number = number.to_string();
        }
        form
    }

    /// Populate the form from an existing binder when editing.
    fn from_binder(binder: &Binder) -> Self {
        Self {
            number: binder.number.to_string(),
            label: binder.label.clone(),
            active: BinderField::Number,
            error: None,
        }
    }

    /// Switch focus to a particular field.
    fn focus(&mut self, field: BinderField) {
        self.active = field;
    }

    /// Swap focus between the number and label fields.
    fn toggle_field(&mut self) {
        self.active = match self.active {
            BinderField::Number => BinderField::Label,
            BinderField::Label => BinderField::Number,
        };
    }

    /// Append a character to the active field, validating allowed input.
    fn push_char(&mut self, ch: char) -> bool {
        match self.active {
            BinderField::Number => {
                if ch.is_ascii_digit() {
                    self.number.push(ch);
                    true
                } else {
                    false
                }
            }
            BinderField::Label => {
                if !ch.is_control() {
                    self.label.push(ch);
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Remove the last character from the active field.
    fn backspace(&mut self) {
        match self.active {
            BinderField::Number => {
                self.number.pop();
            }
            BinderField::Label => {
                self.label.pop();
            }
        }
    }

    /// Validate the inputs and return typed values ready for persistence.
    fn parse_inputs(&self) -> Result<(i64, String)> {
        let number_raw = self.number.trim();
        if number_raw.is_empty() {
            return Err(anyhow!("Binder number is required."));
        }
        let number = number_raw
            .parse::<i64>()
            .context("Binder number must be an integer.")?;
        let label = self.label.trim();
        if label.is_empty() {
            return Err(anyhow!("Binder label is required."));
        }
        Ok((number, label.to_string()))
    }

    /// Render a single line for the form widget, including placeholder styling
    /// and focus highlighting.
    fn build_line(&self, field_name: &str, field: BinderField) -> Line<'static> {
        let (value, is_active) = match field {
            BinderField::Number => (&self.number, self.active == BinderField::Number),
            BinderField::Label => (&self.label, self.active == BinderField::Label),
        };

        let display = if value.is_empty() {
            "<required>".to_string()
        } else {
            value.clone()
        };

        let style = if is_active {
            Style::default().fg(Color::Yellow)
        } else if value.is_empty() {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };

        Line::from(vec![
            Span::raw(format!("{field_name}: ")),
            Span::styled(display, style),
        ])
    }

    /// Return the character count for the requested field. Used to size modal
    /// widgets.
    fn value_len(&self, field: BinderField) -> usize {
        match field {
            BinderField::Number => self.number.chars().count(),
            BinderField::Label => self.label.chars().count(),
        }
    }
}

#[derive(Clone)]
struct ConfirmBinderDelete {
    id: i64,
    number: i64,
    label: String,
}

impl ConfirmBinderDelete {
    /// Build the confirmation state from the binder being considered.
    fn from(binder: Binder) -> Self {
        Self {
            id: binder.id,
            number: binder.number,
            label: binder.label,
        }
    }
}

/// Form state for song creation/editing, including autocomplete tracking.
#[derive(Default, Clone)]
struct SongForm {
    title: String,
    composer: String,
    link: String,
    active: SongField,
    error: Option<String>,
    suggestion: Option<String>,
    autocomplete_disabled: bool,
}

/// Enumerates the fields within the song form to drive focus management.
#[derive(Copy, Clone, PartialEq, Eq)]
enum SongField {
    Title,
    Composer,
    Link,
}

impl Default for SongField {
    fn default() -> Self {
        SongField::Title
    }
}

impl SongForm {
    /// Populate the form from an existing song when entering edit mode.
    fn from_song(song: &Song) -> Self {
        Self {
            title: song.title.clone(),
            composer: song.composer.clone(),
            link: song.link.clone(),
            active: SongField::Title,
            error: None,
            suggestion: None,
            autocomplete_disabled: false,
        }
    }

    /// Cycle focus across the three song fields, clearing autocomplete when the
    /// composer field loses focus.
    fn toggle_field(&mut self) {
        self.active = match self.active {
            SongField::Title => SongField::Composer,
            SongField::Composer => SongField::Link,
            SongField::Link => SongField::Title,
        };
        if self.active != SongField::Composer {
            self.clear_suggestion();
        }
    }

    /// Insert a character into the active field, re-enabling autocomplete for
    /// the composer as soon as the user types.
    fn push_char(&mut self, ch: char) -> bool {
        if ch.is_control() {
            return false;
        }
        match self.active {
            SongField::Title => self.title.push(ch),
            SongField::Composer => {
                self.autocomplete_disabled = false;
                self.composer.push(ch);
            }
            SongField::Link => self.link.push(ch),
        }
        true
    }

    /// Remove a character from the active field, re-enabling composer
    /// suggestions.
    fn backspace(&mut self) {
        match self.active {
            SongField::Title => {
                self.title.pop();
            }
            SongField::Composer => {
                self.composer.pop();
                self.autocomplete_disabled = false;
            }
            SongField::Link => {
                self.link.pop();
            }
        }
    }

    /// Validate and normalize form inputs before they are written to the
    /// database.
    fn parse_inputs(&self) -> Result<(String, String, String)> {
        let title = self.title.trim();
        if title.is_empty() {
            return Err(anyhow!("Song title is required."));
        }
        Ok((
            title.to_string(),
            self.composer.trim().to_string(),
            self.link.trim().to_string(),
        ))
    }

    /// Update the composer autocomplete suggestion based on current input and
    /// the cached composer list.
    fn update_suggestion(&mut self, composers: &[String]) {
        if self.active != SongField::Composer {
            self.clear_suggestion();
            return;
        }

        if self.autocomplete_disabled || self.composer.chars().count() < 2 {
            self.clear_suggestion();
            return;
        }

        let current_lower = self.composer.to_lowercase();
        let maybe_match = composers
            .iter()
            .find(|candidate| candidate.to_lowercase().starts_with(&current_lower));

        if let Some(candidate) = maybe_match {
            if candidate.chars().count() == self.composer.chars().count()
                && candidate.to_lowercase() == current_lower
            {
                self.suggestion = None;
            } else {
                self.suggestion = Some(candidate.clone());
            }
        } else {
            self.suggestion = None;
        }
    }

    /// Apply the suggested composer, marking autocomplete as satisfied so we do
    /// not immediately overwrite the user's choice.
    fn accept_suggestion(&mut self) -> bool {
        if self.suggestion_suffix().is_some() {
            if let Some(candidate) = self.suggestion.clone() {
                self.composer = candidate;
                self.autocomplete_disabled = true;
                self.suggestion = None;
                return true;
            }
        }
        false
    }

    /// Explicitly disable autocomplete for the remainder of this interaction.
    fn cancel_autocomplete(&mut self) -> bool {
        if self.active == SongField::Composer && self.suggestion.is_some() {
            self.autocomplete_disabled = true;
            self.suggestion = None;
            return true;
        }
        false
    }

    /// Drop the current suggestion, typically after the user moves focus.
    fn clear_suggestion(&mut self) {
        self.suggestion = None;
    }

    /// Return the remaining characters to display as a ghosted autocomplete
    /// hint.
    fn suggestion_suffix(&self) -> Option<String> {
        let candidate = self.suggestion.as_ref()?;
        let current_len = self.composer.chars().count();
        let mut chars = candidate.chars();
        for _ in 0..current_len {
            chars.next()?;
        }
        let suffix: String = chars.collect();
        if suffix.is_empty() {
            None
        } else {
            Some(suffix)
        }
    }

    /// Whether we currently have a suggestion to show for the composer field.
    fn has_active_suggestion(&self) -> bool {
        self.active == SongField::Composer && self.suggestion.is_some()
    }

    /// Render a styled line for the modal form, optionally appending the
    /// autocomplete suffix.
    fn build_line(&self, field_name: &str, field: SongField) -> Line<'static> {
        let (value, is_active) = match field {
            SongField::Title => (&self.title, self.active == SongField::Title),
            SongField::Composer => (&self.composer, self.active == SongField::Composer),
            SongField::Link => (&self.link, self.active == SongField::Link),
        };

        let placeholder = match field {
            SongField::Title => "<required>",
            SongField::Composer => "<optional>",
            SongField::Link => "<optional>",
        };

        let display = if value.is_empty() {
            placeholder.to_string()
        } else {
            value.clone()
        };

        let style = if is_active {
            Style::default().fg(Color::Yellow)
        } else if value.is_empty() {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };

        let mut spans = vec![Span::raw(format!("{field_name}: "))];

        if field == SongField::Composer && is_active && !value.is_empty() {
            spans.push(Span::styled(value.clone(), style));
            if let Some(suffix) = self.suggestion_suffix() {
                spans.push(Span::styled(suffix, Style::default().fg(Color::DarkGray)));
            }
        } else {
            spans.push(Span::styled(display, style));
            if field == SongField::Composer && is_active {
                if let Some(suffix) = self.suggestion_suffix() {
                    spans.push(Span::styled(suffix, Style::default().fg(Color::DarkGray)));
                }
            }
        }

        Line::from(spans)
    }

    /// Character length of the requested field.
    fn value_len(&self, field: SongField) -> usize {
        match field {
            SongField::Title => self.title.chars().count(),
            SongField::Composer => self.composer.chars().count(),
            SongField::Link => self.link.chars().count(),
        }
    }
}
/// State for confirming the removal of a song from a specific binder.
struct ConfirmSongRemove {
    binder_id: i64,
    song: Song,
}

/// State for confirming permanent song deletion.
struct ConfirmSongDelete {
    song: Song,
}

/// Tracks the user's choice when leaving the "To Print" flow with unsaved
/// changes.
struct ConfirmToPrintExit {
    exit_app: bool,
    selection: ConfirmPrintChoice,
}

impl ConfirmToPrintExit {
    /// Create a confirmation dialog with the initial selection on "Apply".
    fn new(exit_app: bool) -> Self {
        Self {
            exit_app,
            selection: ConfirmPrintChoice::Apply,
        }
    }

    /// Move the selection forward (Apply → Discard → Cancel).
    fn next(&mut self) {
        self.selection = match self.selection {
            ConfirmPrintChoice::Apply => ConfirmPrintChoice::Discard,
            ConfirmPrintChoice::Discard => ConfirmPrintChoice::Cancel,
            ConfirmPrintChoice::Cancel => ConfirmPrintChoice::Apply,
        };
    }

    /// Move the selection backward (Apply ← Discard ← Cancel).
    fn previous(&mut self) {
        self.selection = match self.selection {
            ConfirmPrintChoice::Apply => ConfirmPrintChoice::Cancel,
            ConfirmPrintChoice::Discard => ConfirmPrintChoice::Apply,
            ConfirmPrintChoice::Cancel => ConfirmPrintChoice::Discard,
        };
    }

    /// Labels rendered on the dialog buttons. They change subtly if the flow is
    /// exiting the entire app.
    fn labels(&self) -> [&'static str; 3] {
        if self.exit_app {
            ["Apply & Quit", "Discard & Quit", "Cancel"]
        } else {
            ["Apply & Leave", "Discard & Leave", "Cancel"]
        }
    }

    /// Index of the currently highlighted choice.
    fn selected_index(&self) -> usize {
        match self.selection {
            ConfirmPrintChoice::Apply => 0,
            ConfirmPrintChoice::Discard => 1,
            ConfirmPrintChoice::Cancel => 2,
        }
    }
}

/// Options presented in the print confirmation dialog.
#[derive(Copy, Clone)]
enum ConfirmPrintChoice {
    Apply,
    Discard,
    Cancel,
}

/// Holds the footer message text plus its severity.
struct StatusMessage {
    text: String,
    kind: StatusKind,
}

/// Severity levels shown in the footer.
enum StatusKind {
    Info,
    Error,
}

impl StatusKind {
    /// Convert the status kind to a Ratatui style.
    fn style(&self) -> Style {
        match self {
            StatusKind::Info => Style::default().fg(Color::Green),
            StatusKind::Error => Style::default().fg(Color::Red),
        }
    }
}

/// Wrapper around the global song list used by the manager screen.
struct SongManagerScreen {
    /// Full backing song list.
    songs: Vec<Song>,
    /// Currently visible (filtered) songs. When no filter is active this is
    /// a clone of `songs`.
    filtered_songs: Vec<Song>,
    /// Optional active filter string.
    filter: Option<String>,
    /// If true, only show songs that do not have a link.
    show_only_no_link: bool,
    /// Selected index into `filtered_songs`.
    selected: usize,
}

impl SongManagerScreen {
    /// Construct the screen and clamp the selection if the incoming list is
    /// empty.
    fn new(songs: Vec<Song>) -> Self {
        let mut screen = Self {
            filtered_songs: Vec::new(),
            songs,
            filter: None,
            show_only_no_link: false,
            selected: 0,
        };
        screen.apply_filter();
        screen.ensure_in_bounds();
        screen
    }

    /// Apply the active filter (or clear it) to produce `filtered_songs`.
    fn apply_filter(&mut self) {
        // Start from the full song list and apply the search query if present.
        let base: Vec<Song> = if let Some(q) = &self.filter {
            let ql = q.to_lowercase();
            if ql.trim().is_empty() {
                self.songs.clone()
            } else {
                self.songs
                    .iter()
                    .filter(|s| {
                        s.title.to_lowercase().contains(&ql)
                            || s.composer.to_lowercase().contains(&ql)
                    })
                    .cloned()
                    .collect()
            }
        } else {
            self.songs.clone()
        };

        // Apply the "no link" filter when enabled.
        if self.show_only_no_link {
            self.filtered_songs = base
                .into_iter()
                .filter(|s| s.link.trim().is_empty())
                .collect();
        } else {
            self.filtered_songs = base;
        }

        if self.filtered_songs.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered_songs.len() {
            self.selected = self.filtered_songs.len() - 1;
        }
    }

    /// Set or clear the filter string and recompute the visible list.
    fn set_filter(&mut self, filter: Option<String>) {
        self.filter = filter;
        self.apply_filter();
    }

    /// Toggle whether only songs without links should be visible.
    fn toggle_show_no_link(&mut self) -> bool {
        self.show_only_no_link = !self.show_only_no_link;
        self.apply_filter();
        self.show_only_no_link
    }

    /// Return the currently highlighted song, if any.
    fn current_song(&self) -> Option<&Song> {
        self.filtered_songs.get(self.selected)
    }

    /// Move selection by `offset`, clamping to the valid range of the
    /// filtered list.
    fn move_selection(&mut self, offset: isize) {
        if self.filtered_songs.is_empty() {
            return;
        }
        let len = self.filtered_songs.len() as isize;
        let mut new = self.selected as isize + offset;
        if new < 0 {
            new = 0;
        }
        if new >= len {
            new = len - 1;
        }
        self.selected = new as usize;
    }

    /// Jump to the first entry.
    fn select_first(&mut self) {
        if !self.filtered_songs.is_empty() {
            self.selected = 0;
        }
    }

    /// Jump to the last entry.
    fn select_last(&mut self) {
        if !self.filtered_songs.is_empty() {
            self.selected = self.filtered_songs.len() - 1;
        }
    }

    /// Replace the backing song list and recompute any active filter.
    fn set_songs(&mut self, songs: Vec<Song>) {
        self.songs = songs;
        self.apply_filter();
    }

    /// Keep the selection index within the filtered list bounds.
    fn ensure_in_bounds(&mut self) {
        if self.filtered_songs.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered_songs.len() {
            self.selected = self.filtered_songs.len() - 1;
        }
    }
}

/// Determines whether the "To Print" screen is grouped by binder or by song.
#[derive(PartialEq, Eq)]
enum ToPrintMode {
    ByBinder,
    BySong,
}

/// All state required to render and interact with the "To Print" report.
struct ToPrintScreen {
    director_exists: bool,
    mode: ToPrintMode,
    binder_reports: Vec<BinderReport>,
    binder_rows: Vec<BinderRow>,
    song_totals: Vec<SongNeeded>,
    song_rows: Vec<String>,
    scroll: u16,
    selected_index: usize,
    pending_changes: usize,
}

impl ToPrintScreen {
    /// Build the screen with the data collected when the director binder is
    /// available.
    fn with_data(binder_reports: Vec<BinderReport>, song_totals: Vec<SongNeeded>) -> Self {
        let mut screen = Self {
            director_exists: true,
            mode: ToPrintMode::ByBinder,
            binder_reports,
            binder_rows: Vec::new(),
            song_totals,
            song_rows: Vec::new(),
            scroll: 0,
            selected_index: 0,
            pending_changes: 0,
        };
        screen.refresh_binder_rows();
        screen.refresh_song_rows();
        screen
    }

    /// Placeholder screen used when the director binder is missing.
    fn missing_director() -> Self {
        Self {
            director_exists: false,
            mode: ToPrintMode::ByBinder,
            binder_reports: Vec::new(),
            binder_rows: Vec::new(),
            song_totals: Vec::new(),
            song_rows: Vec::new(),
            scroll: 0,
            selected_index: 0,
            pending_changes: 0,
        }
    }

    /// Swap between binder-centric and song-centric views.
    fn toggle_mode(&mut self) {
        if !self.director_exists {
            return;
        }
        self.mode = match self.mode {
            ToPrintMode::ByBinder => ToPrintMode::BySong,
            ToPrintMode::BySong => ToPrintMode::ByBinder,
        };
        self.selected_index = 0;
        self.scroll = 0;
        self.update_scroll();
    }

    /// Move the selection pointer, clamping and updating scroll as needed.
    fn move_selection(&mut self, delta: isize) {
        if !self.director_exists {
            return;
        }
        let len = self.current_len();
        if len == 0 {
            self.selected_index = 0;
            self.scroll = 0;
            return;
        }
        let current = self.selected_index as isize;
        let mut new_index = current + delta;
        if new_index < 0 {
            new_index = 0;
        }
        if new_index >= len as isize {
            new_index = len as isize - 1;
        }
        self.selected_index = new_index as usize;
        self.update_scroll();
    }

    /// Jump to the top of the current view.
    fn select_first(&mut self) {
        if !self.director_exists {
            return;
        }
        if self.current_len() == 0 {
            self.selected_index = 0;
        } else {
            self.selected_index = 0;
        }
        self.update_scroll();
    }

    /// Jump to the bottom of the current view.
    fn select_last(&mut self) {
        if !self.director_exists {
            return;
        }
        let len = self.current_len();
        if len == 0 {
            self.selected_index = 0;
        } else {
            self.selected_index = len - 1;
        }
        self.update_scroll();
    }

    /// Generate the printable lines for the current mode, including cursor
    /// pointers.
    fn display_lines(&self) -> Vec<String> {
        if !self.director_exists {
            return Vec::new();
        }

        match self.mode {
            ToPrintMode::ByBinder => {
                if self.binder_rows.is_empty() {
                    let prefix = if self.selected_index == 0 {
                        "▶ "
                    } else {
                        "  "
                    };
                    return vec![format!("{prefix}Nothing to print.")];
                }

                self.binder_rows
                    .iter()
                    .enumerate()
                    .map(|(idx, row)| {
                        let pointer = if idx == self.selected_index {
                            "▶ "
                        } else {
                            "  "
                        };
                        match row.kind {
                            BinderRowKind::Header => format!("{pointer}{}", row.text),
                            BinderRowKind::Song => format!("{pointer}  {}", row.text),
                        }
                    })
                    .collect()
            }
            ToPrintMode::BySong => self
                .song_rows
                .iter()
                .enumerate()
                .map(|(idx, text)| {
                    let pointer = if idx == self.selected_index {
                        "▶ "
                    } else {
                        "  "
                    };
                    format!("{pointer}{text}")
                })
                .collect(),
        }
    }

    /// Toggle the checkbox at the current selection when in binder mode,
    /// returning whether the entry is now checked.
    fn toggle_current(&mut self) -> Option<bool> {
        if !self.director_exists || self.mode != ToPrintMode::ByBinder {
            return None;
        }

        let row = self.binder_rows.get(self.selected_index)?;
        if row.kind != BinderRowKind::Song {
            return None;
        }

        let binder_idx = row.binder_index?;
        let song_idx = row.song_index?;
        let (song_id, now_checked) = {
            let entry = &mut self.binder_reports[binder_idx].songs[song_idx];
            entry.checked = !entry.checked;
            (entry.song.id, entry.checked)
        };

        if now_checked {
            self.pending_changes += 1;
            self.adjust_song_needed(song_id, -1);
        } else {
            self.pending_changes = self.pending_changes.saturating_sub(1);
            self.adjust_song_needed(song_id, 1);
        }
        self.refresh_binder_rows();
        Some(now_checked)
    }

    /// Whether any binder rows have been checked since the last apply.
    fn has_pending_changes(&self) -> bool {
        self.pending_changes > 0
    }

    /// Collect the binder/song pairs that should be applied when the user
    /// confirms.
    fn pending_assignments(&self) -> Vec<(i64, i64)> {
        let mut assignments = Vec::new();
        for report in &self.binder_reports {
            for missing in &report.songs {
                if missing.checked {
                    assignments.push((report.binder_id, missing.song.id));
                }
            }
        }
        assignments
    }

    /// Number of rows in the currently active view.
    fn current_len(&self) -> usize {
        match self.mode {
            ToPrintMode::ByBinder => self.binder_rows.len(),
            ToPrintMode::BySong => self.song_rows.len(),
        }
    }

    /// Maximum scroll offset based on the current view length.
    fn max_scroll(&self) -> u16 {
        if !self.director_exists {
            return 0;
        }
        self.current_len().saturating_sub(1) as u16
    }

    /// Update the scroll offset so the selected row remains near the top of the
    /// viewport.
    fn update_scroll(&mut self) {
        if !self.director_exists {
            self.scroll = 0;
            self.selected_index = 0;
            return;
        }
        let len = self.current_len();
        if len == 0 {
            self.scroll = 0;
            self.selected_index = 0;
            return;
        }
        let desired = self.selected_index.saturating_sub(3) as u16;
        let max_scroll = self.max_scroll();
        self.scroll = min(desired, max_scroll);
    }

    /// Adjust the aggregate song count when a binder row is toggled.
    fn adjust_song_needed(&mut self, song_id: i64, delta: isize) {
        if let Some(entry) = self
            .song_totals
            .iter_mut()
            .find(|entry| entry.song.id == song_id)
        {
            let updated = (entry.needed as isize + delta).max(0) as usize;
            entry.needed = updated;
        }
        self.refresh_song_rows();
    }

    /// Regenerate the textual representation for the song totals view.
    fn refresh_song_rows(&mut self) {
        let mut rows = Vec::new();
        for entry in &self.song_totals {
            if entry.needed > 0 {
                let copies_label = if entry.needed == 1 { "copy" } else { "copies" };
                rows.push(format!(
                    "{}  ({} {})",
                    entry.song.display_title(),
                    entry.needed,
                    copies_label
                ));
            }
        }
        if rows.is_empty() {
            rows.push("No songs need printing.".to_string());
        }
        self.song_rows = rows;
        if matches!(self.mode, ToPrintMode::BySong) {
            let len = self.current_len();
            if len == 0 {
                self.selected_index = 0;
            } else if self.selected_index >= len {
                self.selected_index = len - 1;
            }
            self.update_scroll();
        }
    }

    /// Rebuild the binder rows after toggles or data refreshes.
    fn refresh_binder_rows(&mut self) {
        if !self.director_exists {
            self.binder_rows.clear();
            return;
        }

        let mut rows = Vec::new();
        for (binder_idx, report) in self.binder_reports.iter().enumerate() {
            rows.push(BinderRow {
                kind: BinderRowKind::Header,
                text: format!(
                    "Binder {:02} • {}",
                    report.binder_number, report.binder_label
                ),
                binder_index: Some(binder_idx),
                song_index: None,
            });

            for (song_idx, song) in report.songs.iter().enumerate() {
                let checkbox = if song.checked { "[x]" } else { "[ ]" };
                rows.push(BinderRow {
                    kind: BinderRowKind::Song,
                    text: format!("{} {}", checkbox, song.song.display_title()),
                    binder_index: Some(binder_idx),
                    song_index: Some(song_idx),
                });
            }
        }

        self.binder_rows = rows;
        if matches!(self.mode, ToPrintMode::ByBinder) {
            let len = self.current_len();
            if len == 0 {
                self.selected_index = 0;
            } else if self.selected_index >= len {
                self.selected_index = len - 1;
            }
            self.update_scroll();
        }
    }
}

/// Aggregates missing songs per binder for the "To Print" screen.
struct BinderReport {
    binder_id: i64,
    binder_number: i64,
    binder_label: String,
    songs: Vec<MissingSong>,
}

/// Song that needs to be added to a binder, with a checkbox state.
struct MissingSong {
    song: Song,
    checked: bool,
}

/// Row rendered in the binder list (either a header or an individual song).
struct BinderRow {
    kind: BinderRowKind,
    text: String,
    binder_index: Option<usize>,
    song_index: Option<usize>,
}

/// Distinguishes between binder headers and individual missing songs.
#[derive(PartialEq, Eq)]
enum BinderRowKind {
    Header,
    Song,
}

/// Tracks how many additional copies of a song are required.
struct SongNeeded {
    song: Song,
    needed: usize,
}

/// Backing state for the binder-specific song view.
struct SongScreen {
    binder: Binder,
    /// Full backing song list for this binder.
    songs: Vec<Song>,
    /// Filtered view derived from `songs` when a search query is active.
    filtered_songs: Vec<Song>,
    /// Optional active filter string.
    filter: Option<String>,
    /// Selected index into `filtered_songs`.
    selected: usize,
}

impl SongScreen {
    /// Build the screen state for a binder's song list.
    fn new(binder: Binder, songs: Vec<Song>) -> Self {
        let mut screen = Self {
            binder,
            songs,
            filtered_songs: Vec::new(),
            filter: None,
            selected: 0,
        };
        screen.apply_filter();
        screen.ensure_in_bounds();
        screen
    }

    /// Apply the active filter to produce the `filtered_songs` list.
    fn apply_filter(&mut self) {
        if let Some(q) = &self.filter {
            let ql = q.to_lowercase();
            if ql.trim().is_empty() {
                self.filtered_songs = self.songs.clone();
            } else {
                self.filtered_songs = self
                    .songs
                    .iter()
                    .filter(|s| {
                        s.title.to_lowercase().contains(&ql)
                            || s.composer.to_lowercase().contains(&ql)
                    })
                    .cloned()
                    .collect();
            }
        } else {
            self.filtered_songs = self.songs.clone();
        }

        if self.filtered_songs.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered_songs.len() {
            self.selected = self.filtered_songs.len() - 1;
        }
    }

    /// Set or clear the filter and recompute the visible list.
    fn set_filter(&mut self, filter: Option<String>) {
        self.filter = filter;
        self.apply_filter();
    }

    /// Convenience accessor for the binder id.
    fn binder_id(&self) -> Option<i64> {
        Some(self.binder.id)
    }

    /// Current song selection from the filtered list, if any.
    fn current_song(&self) -> Option<&Song> {
        self.filtered_songs.get(self.selected)
    }

    /// Move the selection within the binder's filtered song list.
    fn move_selection(&mut self, offset: isize) {
        if self.filtered_songs.is_empty() {
            return;
        }
        let len = self.filtered_songs.len() as isize;
        let mut new = self.selected as isize + offset;
        if new < 0 {
            new = 0;
        }
        if new >= len {
            new = len - 1;
        }
        self.selected = new as usize;
    }

    /// Jump to the first song.
    fn select_first(&mut self) {
        if !self.filtered_songs.is_empty() {
            self.selected = 0;
        }
    }

    /// Jump to the last song.
    fn select_last(&mut self) {
        if !self.filtered_songs.is_empty() {
            self.selected = self.filtered_songs.len() - 1;
        }
    }

    /// Replace the song list and clamp the selection.
    fn set_songs(&mut self, songs: Vec<Song>) {
        self.songs = songs;
        self.apply_filter();
    }

    /// Clamp the selection index to a valid song in the filtered list.
    fn ensure_in_bounds(&mut self) {
        if self.filtered_songs.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered_songs.len() {
            self.selected = self.filtered_songs.len() - 1;
        }
    }
}

/// Backing state for the song picker palette when attaching songs to a binder.
struct AddSongState {
    binder_id: i64,
    items: Vec<AddSongItem>,
    selected: usize,
}

/// Entries shown in the song picker list.
enum AddSongItem {
    CreateNew,
    Existing(Song),
}

impl AddSongState {
    /// Build the list of candidates by querying songs not already linked.
    fn load(conn: &Connection, binder_id: i64) -> Result<Self> {
        let mut items = vec![AddSongItem::CreateNew];
        let available = fetch_available_songs(conn, binder_id)?;
        items.extend(available.into_iter().map(AddSongItem::Existing));
        Ok(Self {
            binder_id,
            items,
            selected: 0,
        })
    }

    /// Number of selectable entries.
    fn len(&self) -> usize {
        self.items.len()
    }

    /// Move the highlighted entry within the list.
    fn move_selection(&mut self, offset: isize) {
        if self.items.is_empty() {
            return;
        }
        let len = self.items.len() as isize;
        let mut new = self.selected as isize + offset;
        if new < 0 {
            new = 0;
        }
        if new >= len {
            new = len - 1;
        }
        self.selected = new as usize;
    }

    /// Jump to the first entry.
    fn select_first(&mut self) {
        if !self.items.is_empty() {
            self.selected = 0;
        }
    }

    /// Jump to the final entry.
    fn select_last(&mut self) {
        if !self.items.is_empty() {
            self.selected = self.items.len() - 1;
        }
    }

    /// Currently highlighted item, if the list is not empty.
    fn current_item(&self) -> Option<&AddSongItem> {
        self.items.get(self.selected)
    }
}

/// Produce a rectangle centered within `area` that spans the requested percent
/// of the width and height. Used for modal dialogs.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(area);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(horizontal[1]);

    vertical[1]
}

/// Extract the most relevant error message from a chained error.
fn surface_error(err: &anyhow::Error) -> String {
    err.chain()
        .last()
        .map(|cause| cause.to_string())
        .unwrap_or_else(|| err.to_string())
}
