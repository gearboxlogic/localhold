//! Cross-process coordination for SQLite database replacement.

use std::{
    ffi::OsString,
    fs::{File, OpenOptions, TryLockError},
    path::{Path, PathBuf},
};

use crate::error::StoreError;

/// A process lifetime lease associated with one SQLite database path.
///
/// Normal stores hold a shared lease, so multiple `LocalHold` processes may use
/// WAL concurrency. Restore takes the exclusive form and therefore cannot run
/// while any cooperating `LocalHold` process still has the database open.
#[derive(Debug)]
pub(crate) struct SqliteDatabaseLease {
    _file: File,
}

impl SqliteDatabaseLease {
    pub(crate) fn shared(database_path: &Path) -> Result<Self, StoreError> {
        let file = open_lock_file(database_path)?;
        file.lock_shared().map_err(database_lock_error)?;
        Ok(Self { _file: file })
    }

    pub(crate) fn try_exclusive(database_path: &Path) -> Result<Self, ExclusiveLeaseError> {
        let file = open_lock_file(database_path).map_err(ExclusiveLeaseError::Store)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(ExclusiveLeaseError::InUse),
            Err(TryLockError::Error(error)) => Err(ExclusiveLeaseError::Store(database_lock_error(error))),
        }
    }
}

#[derive(Debug)]
pub(crate) enum ExclusiveLeaseError {
    InUse,
    Store(StoreError),
}

fn lock_path(database_path: &Path) -> Result<PathBuf, std::io::Error> {
    let identity = if database_path.exists() {
        database_path.canonicalize()?
    } else {
        let parent = database_path.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        parent.canonicalize()?.join(
            database_path
                .file_name()
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "database path must name a file"))?,
        )
    };
    let mut path = OsString::from(identity.as_os_str());
    path.push(".localhold.lock");
    Ok(PathBuf::from(path))
}

fn open_lock_file(database_path: &Path) -> Result<File, StoreError> {
    let path = lock_path(database_path).map_err(database_lock_error)?;
    let mut options = OpenOptions::new();
    let _configured = options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let _mode = options.mode(0o600);
    }
    let file = options.open(path).map_err(database_lock_error)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mut permissions = file.metadata().map_err(database_lock_error)?.permissions();
        if permissions.mode() & 0o777 != 0o600 {
            permissions.set_mode(0o600);
            file.set_permissions(permissions).map_err(database_lock_error)?;
        }
    }
    Ok(file)
}

fn database_lock_error(error: std::io::Error) -> StoreError {
    StoreError::Database(Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_leases_coexist_and_block_exclusive_restore() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("memories.db");
        let first = SqliteDatabaseLease::shared(&database).unwrap();
        let second = SqliteDatabaseLease::shared(&database).unwrap();

        assert!(matches!(SqliteDatabaseLease::try_exclusive(&database), Err(ExclusiveLeaseError::InUse)));
        drop(first);
        assert!(matches!(SqliteDatabaseLease::try_exclusive(&database), Err(ExclusiveLeaseError::InUse)));
        drop(second);
        let exclusive = SqliteDatabaseLease::try_exclusive(&database).unwrap();
        drop(exclusive);
    }

    #[test]
    fn lock_path_cannot_collide_with_sqlite_sidecars() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("localhold.db");
        assert_eq!(lock_path(&database).unwrap(), directory.path().join("localhold.db.localhold.lock"));
        assert_ne!(lock_path(&database).unwrap(), directory.path().join("localhold.db-wal"));
        assert_ne!(lock_path(&database).unwrap(), directory.path().join("localhold.db-shm"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_share_one_lock_identity() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("localhold.db");
        let _database_file = File::create(&database).unwrap();
        let alias = directory.path().join("alias.db");
        symlink(&database, &alias).unwrap();
        let _shared = SqliteDatabaseLease::shared(&database).unwrap();
        assert!(matches!(SqliteDatabaseLease::try_exclusive(&alias), Err(ExclusiveLeaseError::InUse)));
    }
}
