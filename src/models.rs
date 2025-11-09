//! Domain models that mirror the SQLite schema and get passed throughout the
//! TUI. The intent is that these types stay light-weight data holders so other
//! layers can focus on presentation and persistence logic. Keeping the
//! commentary here means later refactors can reconstruct the assumptions even
//! if other context is lost.

use std::fmt;

#[derive(Debug, Clone)]
/// Represents a physical binder that choristers use. The `number` provides a
/// stable sorting key, while `label` stores the friendly name shown in the UI.
pub struct Binder {
    /// Primary key from the database. We keep this around even when the UI only
    /// needs display information because edit/delete flows bubble the id back to
    /// the persistence layer.
    pub id: i64,
    /// Human-assigned binder number. We preserve it as an integer so ordering is
    /// numeric instead of lexicographic (Binder 2 comes before Binder 10).
    pub number: i64,
    /// User-facing display label.
    pub label: String,
}

impl fmt::Display for Binder {
    /// Write the binder label to any formatter. Display is implemented so the
    /// type plays nicely with Ratatui widgets that consume strings implicitly.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label)
    }
}

#[derive(Debug, Clone)]
/// In-memory representation of a song. The struct mirrors rows in both the
/// `songs` table and the join table that links songs to binders.
pub struct Song {
    /// Primary key from the SQLite store.
    pub id: i64,
    /// Title displayed in lists and search results.
    pub title: String,
    /// Composer field used both for display and filtering.
    pub composer: String,
    /// Optional URL pointing to an online reference (kept as raw text so we can
    /// store non-web references as well).
    pub link: String,
}

impl Song {
    /// Compose a `Title - Composer` string that gracefully omits the hyphen if
    /// the composer is blank. Many views (auto-complete, binder listings) rely
    /// on this ready-to-use formatting.
    pub fn display_title(&self) -> String {
        if self.composer.trim().is_empty() {
            self.title.clone()
        } else {
            format!("{} - {}", self.title, self.composer)
        }
    }
}
