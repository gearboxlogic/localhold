//! Focused contract tests for the process-wide SQLite extension boundary.

use localhold::store::SqliteStore;
use rusqlite::Connection;

#[test]
fn sqlite_vec_auto_extension_loads_on_subsequent_connections() -> Result<(), Box<dyn std::error::Error>> {
    let _store = SqliteStore::in_memory()?;
    let connection = Connection::open_in_memory()?;
    let version: String = connection.query_row("SELECT vec_version()", [], |row| row.get(0))?;

    if !version.starts_with('v') {
        return Err(format!("sqlite-vec returned an invalid version: {version:?}").into());
    }
    Ok(())
}
