//! Binary entry point that glues the SQLite-backed domain model to the TUI.
//! Summarizing the bootstrapping pipeline here keeps the intent obvious when
//! revisiting the code: we bring up the database, hydrate the initial app
//! state, and drive the Ratatui event loop until the user exits.
use choir_binder_manager::{ensure_schema, fetch_composers, load_or_seed_binders, run_app, App};

/// Initialize persistence, load cached data, and launch the Ratatui event loop.
///
/// Returning a `Result` bubbles up fatal initialization problems (for example
/// the user removing the writable `data/` directory) to the terminal instead of
/// crashing silently.
fn main() -> anyhow::Result<()> {
    let conn = ensure_schema()?;
    let binders = load_or_seed_binders(&conn)?;
    let composers = fetch_composers(&conn)?;

    let mut app = App::new(conn, binders, composers);
    run_app(&mut app)
}
