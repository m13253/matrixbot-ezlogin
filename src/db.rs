use std::ops::{Deref, DerefMut};
use std::path::Path;
use std::sync::Once;

use eyre::{Result, WrapErr, bail};
use rusqlite::OpenFlags;
use tracing::info;

static PRINT_SQLITE_VERSION_ONCE: Once = Once::new();

#[derive(Debug)]
pub struct SQLiteHelper {
    conn: rusqlite::Connection,
}

impl SQLiteHelper {
    pub fn open(path: &Path, allow_create: bool) -> Result<Self> {
        let flags = if allow_create {
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_URI
        };
        let conn = rusqlite::Connection::open_with_flags(path, flags)?;

        conn.execute_batch(
            "PRAGMA locking_mode = EXCLUSIVE;
PRAGMA journal_mode = WAL;
PRAGMA journal_size_limit = 0;
PRAGMA wal_autocheckpoint = 1;
PRAGMA optimize = 0x10002;",
        )?;

        let version: String = conn
            .query_row("SELECT sqlite_version();", (), |row| row.get(0))
            // TODO: If anyone needs programmable detection, transform these ad-hoc errors into named error types.
            .wrap_err("failed to get SQLite version")?;

        PRINT_SQLITE_VERSION_ONCE.call_once(|| info!("SQLite version: {}", version));

        // We need 3.45.0 for the jsonb() function
        if !version_compare::compare_to(&version, "3.45.0", version_compare::Cmp::Ge)
            .unwrap_or(false)
        {
            // TODO: If anyone needs programmable detection, transform these ad-hoc errors into named error types.
            bail!(
                "SQLite version is too old: {}. Try compiling with --features=bundled-sqlite",
                version
            );
        }

        Ok(SQLiteHelper { conn })
    }
}

impl AsMut<rusqlite::Connection> for SQLiteHelper {
    fn as_mut(&mut self) -> &mut rusqlite::Connection {
        &mut self.conn
    }
}

impl AsRef<rusqlite::Connection> for SQLiteHelper {
    fn as_ref(&self) -> &rusqlite::Connection {
        &self.conn
    }
}

impl Deref for SQLiteHelper {
    type Target = rusqlite::Connection;

    fn deref(&self) -> &Self::Target {
        &self.conn
    }
}

impl DerefMut for SQLiteHelper {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.conn
    }
}

impl Drop for SQLiteHelper {
    fn drop(&mut self) {
        _ = self.conn.execute("PRAGMA optimize;", ());
    }
}
