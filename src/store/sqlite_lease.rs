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

    pub(crate) fn shared_existing(database_path: &Path) -> Result<Self, StoreError> {
        let file = open_existing_lock_file(database_path)?;
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
    let identity = database_identity(database_path)?;
    let mut path = OsString::from(identity.as_os_str());
    path.push(".localhold.lock");
    Ok(PathBuf::from(path))
}

pub(crate) fn database_identity(database_path: &Path) -> Result<PathBuf, std::io::Error> {
    let mut candidate = database_path.to_path_buf();
    for _depth in 0_u8..40_u8 {
        match candidate.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = std::fs::read_link(&candidate)?;
                candidate = if target.is_absolute() {
                    target
                } else {
                    candidate
                        .parent()
                        .filter(|path| !path.as_os_str().is_empty())
                        .unwrap_or_else(|| Path::new("."))
                        .join(target)
                };
            }
            Ok(_) => return candidate.canonicalize(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let parent = candidate.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
                return Ok(parent.canonicalize()?.join(
                    candidate
                        .file_name()
                        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "database path must name a file"))?,
                ));
            }
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "database path contains too many symbolic links"))
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

fn open_existing_lock_file(database_path: &Path) -> Result<File, StoreError> {
    let path = lock_path(database_path).map_err(database_lock_error)?;
    OpenOptions::new().read(true).open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            StoreError::Conflict("read-only SQLite access requires an existing LocalHold lease sidecar; start LocalHold once with write access before using hold ui".into())
        } else {
            database_lock_error(error)
        }
    })
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
    fn shared_existing_never_creates_a_lock_file() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("memories.db");
        let _database = File::create(&database).unwrap();
        let lease_path = lock_path(&database).unwrap();

        let error = SqliteDatabaseLease::shared_existing(&database).unwrap_err();
        assert!(error.to_string().contains("requires an existing LocalHold lease sidecar"));
        assert!(!lease_path.exists(), "read-only lease acquisition must not create its sidecar");

        drop(SqliteDatabaseLease::shared(&database).unwrap());
        let existing = SqliteDatabaseLease::shared_existing(&database).unwrap();
        assert!(matches!(SqliteDatabaseLease::try_exclusive(&database), Err(ExclusiveLeaseError::InUse)));
        drop(existing);
    }

    #[test]
    fn lock_path_cannot_collide_with_sqlite_sidecars() {
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("localhold.db");
        let canonical_directory = directory.path().canonicalize().unwrap();
        let path = lock_path(&database).unwrap();
        assert_eq!(path, canonical_directory.join("localhold.db.localhold.lock"));
        assert_ne!(path, canonical_directory.join("localhold.db-wal"));
        assert_ne!(path, canonical_directory.join("localhold.db-shm"));
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

    #[cfg(unix)]
    #[test]
    fn dangling_symlink_keeps_the_same_lock_identity_after_target_creation() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("localhold.db");
        let alias = directory.path().join("alias.db");
        symlink(&database, &alias).unwrap();
        let _shared = SqliteDatabaseLease::shared(&alias).unwrap();
        let _database_file = File::create(&database).unwrap();

        assert!(matches!(SqliteDatabaseLease::try_exclusive(&database), Err(ExclusiveLeaseError::InUse)));
    }
}
