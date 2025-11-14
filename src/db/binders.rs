use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, Error as SqlError, ErrorCode};

use crate::models::Binder;

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
