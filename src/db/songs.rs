use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};

use crate::models::Song;

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
