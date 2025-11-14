use std::cmp::min;
use std::collections::HashSet;
use std::mem;

use anyhow::Result;
use crossterm::event::KeyCode;
use open::that as open_link;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::prelude::*;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use rusqlite::Connection;

use crate::db::{
    add_song_to_binder, create_binder, create_song, delete_binder, delete_song, fetch_all_songs,
    fetch_binders, fetch_composers, fetch_songs_for_binder, remove_song_from_binder, update_binder,
    update_song,
};
use crate::models::{Binder, Song};

use super::forms::{
    BinderField, BinderForm, ConfirmBinderDelete, ConfirmPrintChoice, ConfirmSongDelete,
    ConfirmSongRemove, ConfirmToPrintExit, SongField, SongForm,
};
use super::helpers::{build_binder_cover_lines, centered_rect, surface_error};
use super::screens::{
    AddSongItem, AddSongState, BinderReport, MissingSong, SongManagerScreen, SongNeeded,
    SongScreen, ToPrintMode, ToPrintScreen,
};

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

/// High-level navigation states. Keeping this explicit makes it easy to reason
/// about which rendering path runs and what keyboard shortcuts should do.
enum Screen {
    Binders,
    Songs(SongScreen),
    SongManager(SongManagerScreen),
    ToPrint(ToPrintScreen),
}

/// Fine-grained modes scoped to the current screen.
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
    Searching(SearchState),
}

/// Which screen the search is targeting.
enum SearchTarget {
    Songs,
    SongManager,
}

/// State for an active inline search.
struct SearchState {
    target: SearchTarget,
    query: String,
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
    fn style(&self) -> Style {
        match self {
            StatusKind::Info => Style::default().fg(Color::Green),
            StatusKind::Error => Style::default().fg(Color::Red),
        }
    }
}

/// Central application state shared across the TUI.
pub struct App {
    conn: Connection,
    binders: Vec<Binder>,
    selected: usize,
    composers: Vec<String>,
    screen: Screen,
    mode: Mode,
    status: Option<StatusMessage>,
    saved_search: Option<SearchState>,
}

impl App {
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
        } else if let Some(state) = self.saved_search.take() {
            Ok(Mode::Searching(state))
        } else {
            Ok(Mode::Normal)
        }
    }

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
            KeyCode::Char(' ') => {
                state.toggle_current_selection();
                Ok(Mode::SelectingSong(state))
            }
            KeyCode::Enter => {
                let selections = state.checked_songs();
                if !selections.is_empty() {
                    let mut added = 0usize;
                    for song in selections {
                        if let Err(err) = add_song_to_binder(&self.conn, state.binder_id, song.id) {
                            let message = surface_error(&err);
                            self.set_status(message, StatusKind::Error);
                            if added > 0 {
                                self.refresh_song_screen()?;
                                return Ok(Mode::Normal);
                            } else {
                                return Ok(Mode::SelectingSong(state));
                            }
                        }
                        added += 1;
                    }

                    if added > 0 {
                        self.refresh_song_screen()?;
                        let message = if added == 1 {
                            "Song added to binder.".to_string()
                        } else {
                            format!("Added {added} songs to binder.")
                        };
                        self.set_status(message, StatusKind::Info);
                    }

                    Ok(Mode::Normal)
                } else {
                    match state.current_item() {
                        Some(AddSongItem::CreateNew) => Ok(Mode::CreatingSong {
                            binder_id: Some(state.binder_id),
                            form: SongForm::default(),
                        }),
                        Some(AddSongItem::Existing(song)) => {
                            if let Err(err) =
                                add_song_to_binder(&self.conn, state.binder_id, song.id)
                            {
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
                    }
                }
            }
            _ => Ok(Mode::SelectingSong(state)),
        }
    }

    fn handle_search(&mut self, code: KeyCode, mut state: SearchState) -> Result<Mode> {
        match state.target {
            SearchTarget::SongManager => {
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

    pub(crate) fn draw(&self, frame: &mut Frame) {
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

    pub(crate) fn handle_ctrl_e(&mut self) -> Result<()> {
        if !matches!(self.mode, Mode::Searching(_)) {
            return Ok(());
        }

        let previous = mem::replace(&mut self.mode, Mode::Normal);
        if let Mode::Searching(state) = previous {
            self.saved_search = Some(state);
        }

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

    pub(crate) fn handle_ctrl_l(&mut self) -> Result<()> {
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

    fn footer_instructions(&self) -> Line<'static> {
        let key_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        match (&self.screen, &self.mode) {
            (_, Mode::SelectingSong(_)) => Line::from(vec![
                Span::styled("[↑↓]", key_style),
                Span::raw(" Navigate   "),
                Span::styled("[Space]", key_style),
                Span::raw(" Toggle   "),
                Span::styled("[Enter]", key_style),
                Span::raw(" Add Selected   "),
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
            .enumerate()
            .map(|(index, item)| match item {
                AddSongItem::CreateNew => ListItem::new("Create a new song"),
                AddSongItem::Existing(song) => {
                    let checkbox = if state.is_checked(index) {
                        "[x]"
                    } else {
                        "[ ]"
                    };
                    ListItem::new(format!("{checkbox} {}", song.display_title()))
                }
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

    fn split_rows(&self, area: Rect) -> Vec<Rect> {
        let row_count = self.row_count().max(1) as u16;
        let percent = (100 / row_count).max(1);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![Constraint::Percentage(percent); row_count as usize])
            .split(area);
        chunks.iter().cloned().collect()
    }

    fn split_columns(&self, area: Rect) -> Vec<Rect> {
        let columns = GRID_COLUMNS.max(1) as u16;
        let percent = (100 / columns).max(1);
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(vec![Constraint::Percentage(percent); columns as usize])
            .split(area);
        chunks.iter().cloned().collect()
    }

    fn set_status<S: Into<String>>(&mut self, text: S, kind: StatusKind) {
        self.status = Some(StatusMessage {
            text: text.into(),
            kind,
        });
    }

    fn clear_status(&mut self) {
        self.status = None;
    }

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

    fn open_binder_view(&mut self, binder: Binder) -> Result<()> {
        let songs = fetch_songs_for_binder(&self.conn, binder.id)?;
        self.screen = Screen::Songs(SongScreen::new(binder, songs));
        Ok(())
    }

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

    fn open_song_manager(&mut self) -> Result<()> {
        let songs = fetch_all_songs(&self.conn)?;
        self.reload_composers()?;
        self.screen = Screen::SongManager(SongManagerScreen::new(songs));
        Ok(())
    }

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

    fn refresh_song_screen(&mut self) -> Result<()> {
        if let Screen::Songs(ref mut songs) = self.screen {
            let updated = fetch_songs_for_binder(&self.conn, songs.binder.id)?;
            songs.set_songs(updated);
        }
        Ok(())
    }

    fn refresh_song_manager(&mut self) -> Result<()> {
        if let Screen::SongManager(ref mut manager) = self.screen {
            let updated = fetch_all_songs(&self.conn)?;
            manager.set_songs(updated);
        }
        self.reload_composers()?;
        Ok(())
    }

    fn reload_composers(&mut self) -> Result<()> {
        self.composers = fetch_composers(&self.conn)?;
        Ok(())
    }

    fn next_binder_number(&self) -> i64 {
        self.binders
            .iter()
            .map(|binder| binder.number)
            .max()
            .unwrap_or(0)
            + 1
    }

    fn current_binder(&self) -> Option<&Binder> {
        self.binders.get(self.selected)
    }

    fn binder_count(&self) -> usize {
        self.binders.len()
    }

    fn row_count(&self) -> usize {
        let cols = GRID_COLUMNS.max(1);
        (self.binder_count() + cols - 1) / cols
    }

    fn move_horizontal(&mut self, offset: isize) {
        if matches!(self.screen, Screen::Binders) && !self.binders.is_empty() {
            let new_index = self.selected as isize + offset;
            if (0..self.binder_count() as isize).contains(&new_index) {
                self.selected = new_index as usize;
            }
        }
    }

    fn move_vertical(&mut self, offset: isize) {
        if matches!(self.screen, Screen::Binders) && !self.binders.is_empty() {
            let cols = GRID_COLUMNS as isize;
            let new_index = self.selected as isize + offset * cols;
            if (0..self.binder_count() as isize).contains(&new_index) {
                self.selected = new_index as usize;
            }
        }
    }

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
}
