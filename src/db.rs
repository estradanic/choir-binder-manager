//! Persistence helpers around the embedded SQLite database. Every function in
//! this module tries to encapsulate one query or migration so the rest of the
//! codebase can stay focused on UI state management. Capturing the rationale in
//! comments keeps the intent of each query easy to rediscover when returning to
//! the project.

use std::fs;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, Error as SqlError, ErrorCode};

use crate::models::{Binder, Song};

/// Location of the on-disk SQLite database relative to the project root. We
/// keep it as a constant because several code paths (schema creation, tests,
/// and manual migrations) rely on the exact same string.
const DB_PATH: &str = "data/binders.sqlite";

/// Ensure the database file exists, run lazy migrations, and return a live
/// connection. The function also toggles `PRAGMA foreign_keys = ON` so the
/// referential integrity checks in our schema behave the same during tests and
/// production runs.
pub fn ensure_schema() -> Result<Connection> {
    if let Some(parent) = Path::new(DB_PATH).parent() {
        fs::create_dir_all(parent).context("failed to create data directory")?;
    }

    let conn = Connection::open(DB_PATH).context("failed to open SQLite database")?;
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

/// Load existing binders or seed the default set if the database is empty. The
/// seeding logic currently lives inside `fetch_binders` (which returns an empty
/// list when nothing exists) so this wrapper stays small, but keeping a named
/// function makes the startup flow in `main.rs` easier to read.
pub fn load_or_seed_binders(conn: &Connection) -> Result<Vec<Binder>> {
    fetch_binders(conn)
}

/// Retrieve every binder sorted numerically. The query doubles as the single
/// source of truth for how we order binders in the UI.
pub fn fetch_binders(conn: &Connection) -> Result<Vec<Binder>> {
    let mut stmt = conn
        .prepare("SELECT id, number, label FROM binders ORDER BY number")
        .context("failed to prepare binder query")?;

    let binders = stmt
        .query_map([], |row| {
            Ok(Binder {
                id: row.get(0)?,
                number: row.get(1)?,
                label: row.get(2)?,
            })
        })
        .context("failed to load binders")?
        .collect::<Result<Vec<_>, _>>()
        .context("failed to collect binders")?;

    Ok(binders)
}

/// Insert a new binder row, returning the hydrated struct so the caller can
/// push it straight into the in-memory list.
pub fn create_binder(conn: &Connection, number: i64, label: &str) -> Result<Binder> {
    conn.execute(
        "INSERT INTO binders (number, label) VALUES (?1, ?2)",
        params![number, label],
    )
    .map_err(|err| map_unique_constraint(err, number))
    .context("failed to insert binder")?;

    let id = conn.last_insert_rowid();
    Ok(Binder {
        id,
        number,
        label: label.to_string(),
    })
}

/// Update the number and label for an existing binder. We surface a custom
/// error when nothing was updated so the UI can show a friendly message instead
/// of silently continuing.
pub fn update_binder(conn: &Connection, id: i64, number: i64, label: &str) -> Result<()> {
    let updated = conn
        .execute(
            "UPDATE binders SET number = ?1, label = ?2 WHERE id = ?3",
            params![number, label, id],
        )
        .map_err(|err| map_unique_constraint(err, number))
        .context("failed to update binder")?;

    if updated == 0 {
        Err(anyhow!("Binder not found"))
    } else {
    Ok(())
    }
}

/// Remove a binder row. The database schema cascades to `binder_songs`, so we
/// do not have to delete the join table rows manually.
pub fn delete_binder(conn: &Connection, id: i64) -> Result<()> {
    let deleted = conn
        .execute("DELETE FROM binders WHERE id = ?1", params![id])
        .context("failed to delete binder")?;

    if deleted == 0 {
        Err(anyhow!("Binder not found"))
    } else {
    Ok(())
    }
}

/// Coerce SQLite constraint errors into human-readable messages. Right now the
/// only constraint we guard is the uniqueness of binder numbers, but keeping
/// this helper isolated prepares us for future constraints.
fn map_unique_constraint(err: SqlError, number: i64) -> anyhow::Error {
    if matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::ConstraintViolation)
    ) {
        anyhow!("Binder number {number} already exists.")
    } else {
    err.into()
    }
}

/// Fetch all songs across binders, ordered case-insensitively so mixed-case
/// titles group together in the UI.
pub fn fetch_all_songs(conn: &Connection) -> Result<Vec<Song>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, title, composer, link
             FROM songs
             ORDER BY title COLLATE NOCASE, composer COLLATE NOCASE",
        )
        .context("failed to prepare all songs query")?;

    let songs = stmt
        .query_map([], |row| {
            Ok(Song {
                id: row.get(0)?,
                title: row.get(1)?,
                composer: row.get(2)?,
                link: row.get(3)?,
            })
        })
        .context("failed to iterate songs")?
        .collect::<Result<Vec<_>, _>>()
        .context("failed to collect songs")?;

    Ok(songs)
}

/// Retrieve distinct composers for the auto-complete widget. The ordering sorts
/// by lowercase first but falls back to the original text to keep accents and
/// capitalization intact.
pub fn fetch_composers(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT composer FROM songs
             WHERE composer IS NOT NULL AND composer <> ''
             ORDER BY LOWER(composer), composer",
        )
        .context("failed to prepare composer query")?;

    let mut rows = stmt.query([]).context("failed to execute composer query")?;

    let mut composers = Vec::new();
    while let Some(row) = rows.next().context("failed to fetch composer row")? {
        let composer: String = row.get(0).context("failed to read composer value")?;
        composers.push(composer);
    }

    Ok(composers)
}

/// Get every song linked to a specific binder. Used by the detail view when the
/// user drills into a binder card.
pub fn fetch_songs_for_binder(conn: &Connection, binder_id: i64) -> Result<Vec<Song>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.title, s.composer, s.link
             FROM songs s
             INNER JOIN binder_songs bs ON bs.song_id = s.id
             WHERE bs.binder_id = ?1
             ORDER BY s.title COLLATE NOCASE, s.composer COLLATE NOCASE",
        )
        .context("failed to prepare binder songs query")?;

    let songs = stmt
        .query_map([binder_id], |row| {
            Ok(Song {
                id: row.get(0)?,
                title: row.get(1)?,
                composer: row.get(2)?,
                link: row.get(3)?,
            })
        })
        .context("failed to iterate binder songs")?
        .collect::<Result<Vec<_>, _>>()
        .context("failed to collect binder songs")?;

    Ok(songs)
}

/// Return songs not yet assigned to a given binder, enabling the "Add Song"
/// workflow to show only eligible options.
pub fn fetch_available_songs(conn: &Connection, binder_id: i64) -> Result<Vec<Song>> {
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.title, s.composer, s.link
             FROM songs s
             WHERE NOT EXISTS (
                 SELECT 1 FROM binder_songs bs WHERE bs.song_id = s.id AND bs.binder_id = ?1
             )
             ORDER BY s.title COLLATE NOCASE, s.composer COLLATE NOCASE",
        )
        .context("failed to prepare available songs query")?;

    let songs = stmt
        .query_map([binder_id], |row| {
            Ok(Song {
                id: row.get(0)?,
                title: row.get(1)?,
                composer: row.get(2)?,
                link: row.get(3)?,
            })
        })
        .context("failed to iterate available songs")?
        .collect::<Result<Vec<_>, _>>()
        .context("failed to collect available songs")?;

    Ok(songs)
}

/// Insert a brand new song. We echo the hydrated struct so callers can update
/// UI state without having to re-query the database.
pub fn create_song(conn: &Connection, title: &str, composer: &str, link: &str) -> Result<Song> {
    conn.execute(
        "INSERT INTO songs (title, composer, link) VALUES (?1, ?2, ?3)",
        params![title, composer, link],
    )
    .context("failed to insert song")?;

    let id = conn.last_insert_rowid();
    Ok(Song {
        id,
        title: title.to_string(),
        composer: composer.to_string(),
        link: link.to_string(),
    })
}

/// Update all editable song fields. Like other update helpers, we surface an
/// explicit error when zero rows are touched.
pub fn update_song(
    conn: &Connection,
    id: i64,
    title: &str,
    composer: &str,
    link: &str,
) -> Result<()> {
    let updated = conn
        .execute(
            "UPDATE songs SET title = ?1, composer = ?2, link = ?3 WHERE id = ?4",
            params![title, composer, link, id],
        )
        .context("failed to update song")?;

    if updated == 0 {
        Err(anyhow!("Song not found"))
    } else {
    Ok(())
    }
}

/// Create a link between a binder and a song. Using `INSERT OR IGNORE` lets us
/// treat repeated requests idempotently, which simplifies state management in
/// the UI.
pub fn add_song_to_binder(conn: &Connection, binder_id: i64, song_id: i64) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO binder_songs (binder_id, song_id) VALUES (?1, ?2)",
        params![binder_id, song_id],
    )
    .context("failed to link song to binder")?;
    Ok(())
}

/// Remove a binder-song association and surface a descriptive error if the link
/// never existed.
pub fn remove_song_from_binder(conn: &Connection, binder_id: i64, song_id: i64) -> Result<()> {
    let deleted = conn
        .execute(
            "DELETE FROM binder_songs WHERE binder_id = ?1 AND song_id = ?2",
            params![binder_id, song_id],
        )
        .context("failed to unlink song from binder")?;

    if deleted == 0 {
        Err(anyhow!("Song not linked to this binder"))
    } else {
    Ok(())
    }
}

/// Permanently delete a song. The join table cascades automatically so binders
/// lose the entry without additional cleanup.
pub fn delete_song(conn: &Connection, id: i64) -> Result<()> {
    let deleted = conn
        .execute("DELETE FROM songs WHERE id = ?1", params![id])
        .context("failed to delete song")?;

    if deleted == 0 {
        Err(anyhow!("Song not found"))
    } else {
        Ok(())
    }
}
