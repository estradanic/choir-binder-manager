use std::cmp::{min, Ordering};
use std::collections::HashSet;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::db::fetch_available_songs;
use crate::models::{Binder, Song};

/// Wrapper around the global song list used by the manager screen.
pub(crate) struct SongManagerScreen {
    pub(crate) songs: Vec<Song>,
    pub(crate) filtered_songs: Vec<Song>,
    pub(crate) filter: Option<String>,
    pub(crate) show_only_no_link: bool,
    pub(crate) selected: usize,
}

impl SongManagerScreen {
    pub(crate) fn new(songs: Vec<Song>) -> Self {
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

    pub(crate) fn apply_filter(&mut self) {
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

    pub(crate) fn set_filter(&mut self, filter: Option<String>) {
        self.filter = filter;
        self.apply_filter();
    }

    pub(crate) fn toggle_show_no_link(&mut self) -> bool {
        self.show_only_no_link = !self.show_only_no_link;
        self.apply_filter();
        self.show_only_no_link
    }

    pub(crate) fn current_song(&self) -> Option<&Song> {
        self.filtered_songs.get(self.selected)
    }

    pub(crate) fn move_selection(&mut self, offset: isize) {
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

    pub(crate) fn select_first(&mut self) {
        if !self.filtered_songs.is_empty() {
            self.selected = 0;
        }
    }

    pub(crate) fn select_last(&mut self) {
        if !self.filtered_songs.is_empty() {
            self.selected = self.filtered_songs.len() - 1;
        }
    }

    pub(crate) fn set_songs(&mut self, songs: Vec<Song>) {
        self.songs = songs;
        self.apply_filter();
    }

    pub(crate) fn ensure_in_bounds(&mut self) {
        if self.filtered_songs.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered_songs.len() {
            self.selected = self.filtered_songs.len() - 1;
        }
    }
}

#[derive(PartialEq, Eq)]
pub(crate) enum ToPrintMode {
    ByBinder,
    BySong,
}

/// All state required to render and interact with the "To Print" report.
pub(crate) struct ToPrintScreen {
    pub(crate) director_exists: bool,
    pub(crate) mode: ToPrintMode,
    pub(crate) binder_reports: Vec<BinderReport>,
    pub(crate) binder_rows: Vec<BinderRow>,
    pub(crate) song_totals: Vec<SongNeeded>,
    pub(crate) song_rows: Vec<SongRow>,
    pub(crate) scroll: u16,
    pub(crate) selected_index: usize,
    pub(crate) pending_changes: usize,
}

impl ToPrintScreen {
    pub(crate) fn with_data(
        binder_reports: Vec<BinderReport>,
        song_totals: Vec<SongNeeded>,
    ) -> Self {
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

    pub(crate) fn missing_director() -> Self {
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

    pub(crate) fn toggle_mode(&mut self) {
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

    pub(crate) fn move_selection(&mut self, delta: isize) {
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

    pub(crate) fn select_first(&mut self) {
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

    pub(crate) fn select_last(&mut self) {
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

    pub(crate) fn display_lines(&self) -> Vec<String> {
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
                .map(|(idx, row)| {
                    let pointer = if idx == self.selected_index {
                        "▶ "
                    } else {
                        "  "
                    };
                    format!("{pointer}{}", row.text)
                })
                .collect(),
        }
    }

    pub(crate) fn toggle_current(&mut self) -> Option<bool> {
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

    pub(crate) fn has_pending_changes(&self) -> bool {
        self.pending_changes > 0
    }

    pub(crate) fn pending_assignments(&self) -> Vec<(i64, i64)> {
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

    pub(crate) fn current_len(&self) -> usize {
        match self.mode {
            ToPrintMode::ByBinder => self.binder_rows.len(),
            ToPrintMode::BySong => self.song_rows.len(),
        }
    }

    pub(crate) fn max_scroll(&self) -> u16 {
        if !self.director_exists {
            return 0;
        }
        self.current_len().saturating_sub(1) as u16
    }

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

    pub(crate) fn refresh_song_rows(&mut self) {
        let mut rows = Vec::new();
        let mut needs: Vec<&SongNeeded> = self
            .song_totals
            .iter()
            .filter(|entry| entry.needed > 0)
            .collect();
        needs.sort_by(|a, b| {
            let title_order = a
                .song
                .title
                .to_lowercase()
                .cmp(&b.song.title.to_lowercase());
            if title_order != Ordering::Equal {
                return title_order;
            }
            a.song
                .composer
                .to_lowercase()
                .cmp(&b.song.composer.to_lowercase())
        });

        for entry in needs {
            rows.push(SongRow::from_needed(entry));
        }
        if rows.is_empty() {
            rows.push(SongRow::placeholder("No songs need printing."));
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

    pub(crate) fn current_song(&self) -> Option<&Song> {
        if !self.director_exists || !matches!(self.mode, ToPrintMode::BySong) {
            return None;
        }
        self.song_rows
            .get(self.selected_index)
            .and_then(|row| row.song.as_ref())
    }

    pub(crate) fn refresh_binder_rows(&mut self) {
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
pub(crate) struct BinderReport {
    pub(crate) binder_id: i64,
    pub(crate) binder_number: i64,
    pub(crate) binder_label: String,
    pub(crate) songs: Vec<MissingSong>,
}

/// Song that needs to be added to a binder, with a checkbox state.
pub(crate) struct MissingSong {
    pub(crate) song: Song,
    pub(crate) checked: bool,
}

/// Row rendered in the binder list (either a header or an individual song).
pub(crate) struct BinderRow {
    pub(crate) kind: BinderRowKind,
    pub(crate) text: String,
    pub(crate) binder_index: Option<usize>,
    pub(crate) song_index: Option<usize>,
}

#[derive(PartialEq, Eq)]
pub(crate) enum BinderRowKind {
    Header,
    Song,
}

/// Tracks how many additional copies of a song are required.
pub(crate) struct SongNeeded {
    pub(crate) song: Song,
    pub(crate) needed: usize,
}

/// Row rendered in the aggregated song view.
pub(crate) struct SongRow {
    pub(crate) text: String,
    pub(crate) song: Option<Song>,
}

impl SongRow {
    fn from_needed(entry: &SongNeeded) -> Self {
        let copies_label = if entry.needed == 1 { "copy" } else { "copies" };
        Self {
            text: format!(
                "{}  ({} {})",
                entry.song.display_title(),
                entry.needed,
                copies_label
            ),
            song: Some(entry.song.clone()),
        }
    }

    fn placeholder(text: &str) -> Self {
        Self {
            text: text.to_string(),
            song: None,
        }
    }
}

/// Backing state for the binder-specific song view.
pub(crate) struct SongScreen {
    pub(crate) binder: Binder,
    pub(crate) songs: Vec<Song>,
    pub(crate) filtered_songs: Vec<Song>,
    pub(crate) filter: Option<String>,
    pub(crate) selected: usize,
}

impl SongScreen {
    pub(crate) fn new(binder: Binder, songs: Vec<Song>) -> Self {
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

    pub(crate) fn set_filter(&mut self, filter: Option<String>) {
        self.filter = filter;
        self.apply_filter();
    }

    pub(crate) fn binder_id(&self) -> Option<i64> {
        Some(self.binder.id)
    }

    pub(crate) fn current_song(&self) -> Option<&Song> {
        self.filtered_songs.get(self.selected)
    }

    pub(crate) fn move_selection(&mut self, offset: isize) {
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

    pub(crate) fn select_first(&mut self) {
        if !self.filtered_songs.is_empty() {
            self.selected = 0;
        }
    }

    pub(crate) fn select_last(&mut self) {
        if !self.filtered_songs.is_empty() {
            self.selected = self.filtered_songs.len() - 1;
        }
    }

    pub(crate) fn set_songs(&mut self, songs: Vec<Song>) {
        self.songs = songs;
        self.apply_filter();
    }

    fn ensure_in_bounds(&mut self) {
        if self.filtered_songs.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered_songs.len() {
            self.selected = self.filtered_songs.len() - 1;
        }
    }
}

/// Backing state for the song picker palette when attaching songs to a binder.
pub(crate) struct AddSongState {
    pub(crate) binder_id: i64,
    pub(crate) items: Vec<AddSongItem>,
    pub(crate) selected: usize,
    pub(crate) checked: HashSet<i64>,
    pub(crate) director_song_ids: HashSet<i64>,
}

/// Entries shown in the song picker list.
#[derive(Clone)]
pub(crate) enum AddSongItem {
    CreateNew,
    Existing(Song),
}

impl AddSongState {
    pub(crate) fn load(conn: &Connection, binder_id: i64) -> Result<Self> {
        let mut items = vec![AddSongItem::CreateNew];
        let available = fetch_available_songs(conn, binder_id)?;
        let director_song_ids = Self::load_director_song_ids(conn)?;
        items.extend(available.into_iter().map(AddSongItem::Existing));
        Ok(Self {
            binder_id,
            items,
            selected: 0,
            checked: HashSet::new(),
            director_song_ids,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn move_selection(&mut self, offset: isize) {
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

    pub(crate) fn select_first(&mut self) {
        if !self.items.is_empty() {
            self.selected = 0;
        }
    }

    pub(crate) fn select_last(&mut self) {
        if !self.items.is_empty() {
            self.selected = self.items.len() - 1;
        }
    }

    pub(crate) fn current_item(&self) -> Option<&AddSongItem> {
        self.items.get(self.selected)
    }

    pub(crate) fn is_checked(&self, index: usize) -> bool {
        matches!(
            self.items.get(index),
            Some(AddSongItem::Existing(song)) if self.checked.contains(&song.id)
        )
    }

    pub(crate) fn toggle_current_selection(&mut self) {
        if let Some(AddSongItem::Existing(song)) = self.items.get(self.selected) {
            if !self.checked.remove(&song.id) {
                self.checked.insert(song.id);
            }
        }
    }

    pub(crate) fn checked_songs(&self) -> Vec<Song> {
        self.items
            .iter()
            .filter_map(|item| match item {
                AddSongItem::Existing(song) if self.checked.contains(&song.id) => {
                    Some(song.clone())
                }
                _ => None,
            })
            .collect()
    }

    fn load_director_song_ids(conn: &Connection) -> Result<HashSet<i64>> {
        let mut stmt = conn
            .prepare(
                "SELECT bs.song_id
                 FROM binder_songs bs
                 INNER JOIN binders b ON b.id = bs.binder_id
                 WHERE b.number = 0",
            )
            .context("failed to prepare director song lookup")?;
        let ids = stmt
            .query_map([], |row| row.get(0))
            .context("failed to iterate director songs")?
            .collect::<rusqlite::Result<Vec<i64>>>()
            .context("failed to collect director song ids")?;
        Ok(ids.into_iter().collect())
    }
}
