use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use directories::BaseDirs;
use rusqlite::Connection;

/// Folder name used beneath the user's home directory for application data.
const DATA_DIR_NAME: &str = ".choir-binder-manager";
/// SQLite file name stored inside the application data directory.
const DB_FILE_NAME: &str = "binders.sqlite";

/// Ensure the database file exists, run lazy migrations, and return a live
/// connection. The function also toggles `PRAGMA foreign_keys = ON` so the
/// referential integrity checks in our schema behave the same during tests and
/// production runs.
pub fn ensure_schema() -> Result<Connection> {
    let db_path = db_path()?;

    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent).context("failed to create data directory")?;
    }

    let conn = Connection::open(&db_path).context("failed to open SQLite database")?;
    conn.execute("PRAGMA foreign_keys = ON", [])
        .context("failed to enable foreign keys")?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS binders (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            number INTEGER NOT NULL UNIQUE,
            label TEXT NOT NULL
        )",
        [],
    )
    .context("failed to create binders table")?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS songs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            title TEXT NOT NULL,
            composer TEXT,
            link TEXT
        )",
        [],
    )
    .context("failed to create songs table")?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS binder_songs (
            binder_id INTEGER NOT NULL,
            song_id INTEGER NOT NULL,
            PRIMARY KEY (binder_id, song_id),
            FOREIGN KEY(binder_id) REFERENCES binders(id) ON DELETE CASCADE,
            FOREIGN KEY(song_id) REFERENCES songs(id) ON DELETE CASCADE
        )",
        [],
    )
    .context("failed to create binder_songs table")?;

    Ok(conn)
}

/// Resolve the absolute path to the SQLite database inside the user's home.
fn db_path() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().ok_or_else(|| anyhow!("could not locate home directory"))?;
    Ok(base_dirs.home_dir().join(DATA_DIR_NAME).join(DB_FILE_NAME))
}
