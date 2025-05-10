use std::path::Path;
use std::sync::Once;

use color_eyre::eyre::{Result, WrapErr, bail};
use rusqlite::OpenFlags;
use tracing::info;

mod duplex_log;
mod interactive;
mod login;
mod sync;

pub use duplex_log::DuplexLog;
pub use interactive::setup_interactive;
pub use login::SetupConfig;
pub use login::login;
pub use login::setup;
pub use sync::SyncHelper;

static PRINT_SQLITE_VERSION_ONCE: Once = Once::new();

fn connect_sqlite(path: &Path, flags: OpenFlags) -> Result<rusqlite::Connection> {
    let conn = rusqlite::Connection::open_with_flags(path, flags)?;

    let version: String = conn
        .query_row("SELECT sqlite_version();", (), |row| row.get(0))
        .wrap_err("failed to get SQLite version")?;
    PRINT_SQLITE_VERSION_ONCE.call_once(|| info!("SQLite version: {version}"));
    if !version_compare::compare_to(&version, "3.45.0", version_compare::Cmp::Ge).unwrap_or(false) {
        bail!("SQLite version is too old: {version}. Please upgrade to 3.45.0 or newer");
    }

    Ok(conn)
}
