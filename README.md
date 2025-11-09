# Choir Binder Manager

A Rust-based terminal user interface for tracking choir binders and their sheet music, backed by SQLite.

## Prerequisites

- Rust toolchain (1.74 or newer recommended)
- SQLite development libraries (optional when using the bundled feature)

## Getting Started

```bash
cargo run
```

The application creates a local SQLite database at `data/binders.sqlite` on first launch and seeds 20 binders numbered 1 through 20. Use the arrow keys to navigate the grid and press `q` or `Esc` to exit.

## Project Layout

- `src/db.rs` – SQLite setup and data-loading utilities
- `src/models.rs` – Core data models for binders and songs
- `src/ui.rs` – Ratatui-based terminal UI

## Next Steps

- Add views for individual binders and song lists
- Implement CRUD operations for songs and binder assignments
- Integrate search and filtering capabilities
