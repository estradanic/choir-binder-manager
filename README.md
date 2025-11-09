# Choir Binder Manager

A Rust-based terminal user interface for tracking choir binders and their sheet music, backed by SQLite.

## Prerequisites

- Rust toolchain (1.74 or newer recommended)
- SQLite development libraries (optional when using the bundled feature)

## Getting Started

```bash
cargo run
```

The application creates a local SQLite database at `~/.choir-binder-manager/binders.sqlite` on first launch. Use the arrow keys to navigate the grid and press `q` or `Esc` to exit.

Keyboard notes:

- Press `f` while viewing a song list (either inside a binder or the global Song Manager) to open an inline search bar at the top of the screen.
- Type to filter by song title or composer (case-insensitive substring match). Use Up/Down to navigate the filtered results.
- Press `Esc` to exit the search and clear the filter.
- In the Song Manager, press `l` to toggle showing only songs without links. The shortcut also works with `Ctrl+L` while the search bar is open.

## Project Layout

- `src/db.rs` – SQLite setup and data-loading utilities
- `src/models.rs` – Core data models for binders and songs
- `src/ui.rs` – Ratatui-based terminal UI

## Next Steps

- Integrate search and filtering capabilities
