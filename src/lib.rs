//! Core library surface for the Choir Binder Manager TUI application.
//!
//! The public modules exposed here provide an intentionally small API so the
//! `bin` target as well as potential external tooling can reuse the same pieces.
//! Keeping the glue logic documented makes it easy to recall why each re-export
//! exists when revisiting the project.
pub mod db;
pub mod models;
pub mod ui;

/// Convenience re-exports for the persistence layer. These functions are
/// typically used by `main.rs` to initialize the embedded SQLite store and
/// preload data.
pub use db::{ensure_schema, fetch_composers, fetch_binders};

/// The two primary domain types that other layers manipulate.
pub use models::{Binder, Song};

/// The interactive application entry point and state container.
pub use ui::{run_app, App};
