use anyhow::{Context, Result};
use redb::{Database, TableDefinition, TableError};
use std::fs;
use std::path::Path;
use std::sync::Arc;

/// Unified host metadata table stored inside the runtime `redb` database (`var/dambo.redb`).
pub const APP_META_TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("app_meta");

/// Key in [`APP_META_TABLE`] holding the JSON-serialized Bilibili [`SessionData`](crate::auth::SessionData).
pub const KEY_AUTH_SESSION: &str = "auth_session";

/// Marker key in [`APP_META_TABLE`] recorded once legacy `session.json` migration has been applied.
pub const KEY_AUTH_SESSION_MIGRATED: &str = "auth_session_migrated";

/// Key in [`APP_META_TABLE`] holding the JSON-serialized [`AppPersistedState`](crate::state::AppPersistedState).
pub const KEY_APP_STATE: &str = "app_state";

/// Marker key in [`APP_META_TABLE`] recorded once legacy `state.json` migration has been applied.
pub const KEY_APP_STATE_MIGRATED: &str = "app_state_migrated";

/// Open or create the shared runtime `redb` database at `db_path`, restrict permissions on Unix,
/// and ensure [`APP_META_TABLE`] exists.
pub fn open_database(db_path: &Path) -> Result<Arc<Database>> {
    if let Some(parent) = db_path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create database parent directory {:?}", parent))?;
    }

    let db = Database::create(db_path).with_context(|| {
        format!(
            "Failed to open redb database at {:?} (ensure no other dambo process is using it)",
            db_path
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = fs::set_permissions(db_path, fs::Permissions::from_mode(0o600)) {
            log::warn!(
                "Failed to restrict permissions (0600) on {:?}: {}",
                db_path,
                e
            );
        }
    }

    let write_txn = db.begin_write()?;
    {
        let _ = write_txn.open_table(APP_META_TABLE)?;
    }
    write_txn.commit()?;

    Ok(Arc::new(db))
}

/// Open the runtime `redb` database at `db_path`, transparently migrating a legacy plugin database
/// (`var/plugins.redb`) to `db_path` if `db_path` does not yet exist.
pub fn open_runtime_database(
    db_path: &Path,
    legacy_plugin_db_path: Option<&Path>,
) -> Result<Arc<Database>> {
    if !db_path.exists()
        && let Some(legacy) = legacy_plugin_db_path
        && legacy != db_path
        && legacy.exists()
    {
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create database parent directory {:?}", parent)
            })?;
        }
        if let Err(rename_err) = fs::rename(legacy, db_path) {
            fs::copy(legacy, db_path).with_context(|| {
                format!(
                    "Failed to migrate legacy plugin database {:?} -> {:?} (rename error: {})",
                    legacy, db_path, rename_err
                )
            })?;
            if let Err(remove_err) = fs::remove_file(legacy) {
                log::warn!(
                    "Copied legacy plugin database {:?} -> {:?}, but failed to remove legacy file: {}",
                    legacy,
                    db_path,
                    remove_err
                );
            }
        }
        log::info!(
            "Migrated legacy plugin database {:?} -> {:?}",
            legacy,
            db_path
        );
    }

    open_database(db_path)
}

/// Read a raw byte slice for `key` from [`APP_META_TABLE`], returning `None` if absent.
pub fn read_meta(db: &Database, key: &str) -> Result<Option<Vec<u8>>> {
    let read_txn = db.begin_read()?;
    let table = match read_txn.open_table(APP_META_TABLE) {
        Ok(table) => table,
        Err(TableError::TableDoesNotExist(_)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    Ok(table.get(key)?.map(|guard| guard.value().to_vec()))
}

/// Write a single `(key, value)` entry to [`APP_META_TABLE`].
pub fn write_meta(db: &Database, key: &str, value: &[u8]) -> Result<()> {
    write_meta_batch(db, &[(key, value)])
}

/// Atomically write multiple `(key, value)` entries to [`APP_META_TABLE`] in a single commit.
pub fn write_meta_batch(db: &Database, entries: &[(&str, &[u8])]) -> Result<()> {
    let write_txn = db.begin_write()?;
    {
        let mut table = write_txn.open_table(APP_META_TABLE)?;
        for &(key, value) in entries {
            table.insert(key, value)?;
        }
    }
    write_txn.commit()?;
    Ok(())
}

/// Remove `key` from [`APP_META_TABLE`].
pub fn delete_meta(db: &Database, key: &str) -> Result<()> {
    let write_txn = db.begin_write()?;
    {
        let mut table = write_txn.open_table(APP_META_TABLE)?;
        table.remove(key)?;
    }
    write_txn.commit()?;
    Ok(())
}
