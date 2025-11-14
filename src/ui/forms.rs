use anyhow::{anyhow, Context, Result};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::models::{Binder, Song};

/// Internal representation of the "binder" form fields.
#[derive(Default, Clone)]
pub(crate) struct BinderForm {
    pub(crate) number: String,
    pub(crate) label: String,
    pub(crate) active: BinderField,
    pub(crate) error: Option<String>,
}

/// Fields available within the binder form.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum BinderField {
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
    pub(crate) fn with_number(number: i64) -> Self {
        let mut form = Self::default();
        if number > 0 {
            form.number = number.to_string();
        }
        form
    }

    /// Populate the form from an existing binder when editing.
    pub(crate) fn from_binder(binder: &Binder) -> Self {
        Self {
            number: binder.number.to_string(),
            label: binder.label.clone(),
            active: BinderField::Number,
            error: None,
        }
    }

    /// Switch focus to a particular field.
    pub(crate) fn focus(&mut self, field: BinderField) {
        self.active = field;
    }

    /// Swap focus between the number and label fields.
    pub(crate) fn toggle_field(&mut self) {
        self.active = match self.active {
            BinderField::Number => BinderField::Label,
            BinderField::Label => BinderField::Number,
        };
    }

    /// Append a character to the active field, validating allowed input.
    pub(crate) fn push_char(&mut self, ch: char) -> bool {
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
    pub(crate) fn backspace(&mut self) {
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
    pub(crate) fn parse_inputs(&self) -> Result<(i64, String)> {
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

    /// Render a single line for the form widget.
    pub(crate) fn build_line(&self, field_name: &str, field: BinderField) -> Line<'static> {
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

    /// Return the character count for the requested field.
    pub(crate) fn value_len(&self, field: BinderField) -> usize {
        match field {
            BinderField::Number => self.number.chars().count(),
            BinderField::Label => self.label.chars().count(),
        }
    }
}

#[derive(Clone)]
pub(crate) struct ConfirmBinderDelete {
    pub(crate) id: i64,
    pub(crate) number: i64,
    pub(crate) label: String,
}

impl ConfirmBinderDelete {
    /// Build the confirmation state from the binder being considered.
    pub(crate) fn from(binder: Binder) -> Self {
        Self {
            id: binder.id,
            number: binder.number,
            label: binder.label,
        }
    }
}

/// Form state for song creation/editing, including autocomplete tracking.
#[derive(Default, Clone)]
pub(crate) struct SongForm {
    pub(crate) title: String,
    pub(crate) composer: String,
    pub(crate) link: String,
    pub(crate) active: SongField,
    pub(crate) error: Option<String>,
    pub(crate) suggestion: Option<String>,
    pub(crate) autocomplete_disabled: bool,
}

/// Enumerates the fields within the song form to drive focus management.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum SongField {
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
    pub(crate) fn from_song(song: &Song) -> Self {
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

    /// Cycle focus across the three song fields.
    pub(crate) fn toggle_field(&mut self) {
        self.active = match self.active {
            SongField::Title => SongField::Composer,
            SongField::Composer => SongField::Link,
            SongField::Link => SongField::Title,
        };
        if self.active != SongField::Composer {
            self.clear_suggestion();
        }
    }

    /// Insert a character into the active field.
    pub(crate) fn push_char(&mut self, ch: char) -> bool {
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

    /// Remove a character from the active field.
    pub(crate) fn backspace(&mut self) {
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
    pub(crate) fn parse_inputs(&self) -> Result<(String, String, String)> {
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

    /// Update the composer autocomplete suggestion based on current input.
    pub(crate) fn update_suggestion(&mut self, composers: &[String]) {
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

    /// Apply the suggested composer, marking autocomplete as satisfied.
    pub(crate) fn accept_suggestion(&mut self) -> bool {
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

    /// Explicitly disable autocomplete for the rest of this interaction.
    pub(crate) fn cancel_autocomplete(&mut self) -> bool {
        if self.active == SongField::Composer && self.suggestion.is_some() {
            self.autocomplete_disabled = true;
            self.suggestion = None;
            return true;
        }
        false
    }

    /// Drop the current suggestion.
    fn clear_suggestion(&mut self) {
        self.suggestion = None;
    }

    /// Return the remaining characters to display as a ghosted autocomplete
    /// hint.
    pub(crate) fn suggestion_suffix(&self) -> Option<String> {
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
    pub(crate) fn has_active_suggestion(&self) -> bool {
        self.active == SongField::Composer && self.suggestion.is_some()
    }

    /// Render a styled line for the modal form, optionally appending the
    /// autocomplete suffix.
    pub(crate) fn build_line(&self, field_name: &str, field: SongField) -> Line<'static> {
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
    pub(crate) fn value_len(&self, field: SongField) -> usize {
        match field {
            SongField::Title => self.title.chars().count(),
            SongField::Composer => self.composer.chars().count(),
            SongField::Link => self.link.chars().count(),
        }
    }
}

/// State for confirming the removal of a song from a specific binder.
pub(crate) struct ConfirmSongRemove {
    pub(crate) binder_id: i64,
    pub(crate) song: Song,
}

/// State for confirming permanent song deletion.
pub(crate) struct ConfirmSongDelete {
    pub(crate) song: Song,
}

/// Tracks the user's choice when leaving the "To Print" flow with unsaved
/// changes.
pub(crate) struct ConfirmToPrintExit {
    pub(crate) exit_app: bool,
    pub(crate) selection: ConfirmPrintChoice,
}

impl ConfirmToPrintExit {
    /// Create a confirmation dialog with the initial selection on "Apply".
    pub(crate) fn new(exit_app: bool) -> Self {
        Self {
            exit_app,
            selection: ConfirmPrintChoice::Apply,
        }
    }

    /// Move the selection forward (Apply → Discard → Cancel).
    pub(crate) fn next(&mut self) {
        self.selection = match self.selection {
            ConfirmPrintChoice::Apply => ConfirmPrintChoice::Discard,
            ConfirmPrintChoice::Discard => ConfirmPrintChoice::Cancel,
            ConfirmPrintChoice::Cancel => ConfirmPrintChoice::Apply,
        };
    }

    /// Move the selection backward (Apply ← Discard ← Cancel).
    pub(crate) fn previous(&mut self) {
        self.selection = match self.selection {
            ConfirmPrintChoice::Apply => ConfirmPrintChoice::Cancel,
            ConfirmPrintChoice::Discard => ConfirmPrintChoice::Apply,
            ConfirmPrintChoice::Cancel => ConfirmPrintChoice::Discard,
        };
    }

    /// Labels rendered on the dialog buttons.
    pub(crate) fn labels(&self) -> [&'static str; 3] {
        if self.exit_app {
            ["Apply & Quit", "Discard & Quit", "Cancel"]
        } else {
            ["Apply & Leave", "Discard & Leave", "Cancel"]
        }
    }

    /// Index of the currently highlighted choice.
    pub(crate) fn selected_index(&self) -> usize {
        match self.selection {
            ConfirmPrintChoice::Apply => 0,
            ConfirmPrintChoice::Discard => 1,
            ConfirmPrintChoice::Cancel => 2,
        }
    }
}

/// Options presented in the print confirmation dialog.
#[derive(Copy, Clone)]
pub(crate) enum ConfirmPrintChoice {
    Apply,
    Discard,
    Cancel,
}
