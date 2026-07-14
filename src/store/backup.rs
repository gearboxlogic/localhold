//! Supported WAL-safe SQLite backup and restore operations.

use std::{
    ffi::OsString,
    fmt::Write as _,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use rusqlite::{Connection, OpenFlags, backup::StepResult};
use serde::Serialize;

use super::{
    EmbeddingProfile, SqliteStore, existing_embedding_dimensions,
    migration::validate_sqlite_source_schema,
    schema::SQLITE_SCHEMA_VERSION,
    sqlite::read_embedding_profile,
    sqlite_lease::{ExclusiveLeaseError, SqliteDatabaseLease},
};
use crate::{
    clock::{Clock, SystemClock},
    error::StoreError,
};

const COPY_PAGES_PER_STEP: i32 = 128;
const COPY_RETRY_DELAY: Duration = Duration::from_millis(25);
const COPY_TIMEOUT: Duration = Duration::from_secs(30);
const REPORT_SCHEMA_VERSION: u32 = 1;

static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Options for creating a supported SQLite backup.
#[derive(Clone, Debug)]
pub struct BackupOptions {
    database_path: PathBuf,
    destination: PathBuf,
    clock: Arc<dyn Clock>,
}

impl BackupOptions {
    /// Create backup options using the production clock.
    #[must_use]
    pub fn new(database_path: PathBuf, destination: PathBuf) -> Self {
        Self {
            database_path,
            destination,
            clock: Arc::new(SystemClock::new()),
        }
    }

    /// Override the clock used for retry deadlines and report timestamps.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

/// Options for validating or applying a supported SQLite restore.
#[derive(Clone, Debug)]
pub struct RestoreOptions {
    database_path: PathBuf,
    backup_path: PathBuf,
    embedding_dimensions: usize,
    expected_profile: Option<EmbeddingProfile>,
    dry_run: bool,
    confirmed: bool,
    clock: Arc<dyn Clock>,
}

impl RestoreOptions {
    /// Create restore options using the production clock.
    #[must_use]
    pub fn new(database_path: PathBuf, backup_path: PathBuf, embedding_dimensions: usize, expected_profile: Option<EmbeddingProfile>) -> Self {
        Self {
            database_path,
            backup_path,
            embedding_dimensions,
            expected_profile,
            dry_run: false,
            confirmed: false,
            clock: Arc::new(SystemClock::new()),
        }
    }

    /// Request validation and coordination checks without replacing data.
    #[must_use]
    pub const fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Record the operator's explicit confirmation for a destructive restore.
    #[must_use]
    pub const fn confirmed(mut self, confirmed: bool) -> Self {
        self.confirmed = confirmed;
        self
    }

    /// Override the clock used for retry deadlines and report timestamps.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }
}

/// Stable machine-readable outcome of a backup or restore operation.
#[derive(Clone, Debug, Serialize)]
#[non_exhaustive]
pub struct MaintenanceReport {
    /// Report contract version.
    pub schema_version: u32,
    /// `backup` or `restore`.
    pub operation: &'static str,
    /// Stable outcome category.
    pub status: &'static str,
    /// Source database or backup path.
    pub source: String,
    /// Destination backup or database path.
    pub destination: String,
    /// Whether this was a restore validation dry run.
    pub dry_run: bool,
    /// Whether the configured database was replaced.
    pub database_replaced: bool,
    /// Retained pre-restore snapshot, when replacement occurred.
    pub recovery_path: Option<String>,
    /// Validated `LocalHold` SQLite schema version.
    pub database_schema_version: Option<u32>,
    /// Validated embedding profile stored in the backup.
    pub embedding_profile: Option<EmbeddingProfile>,
    /// Number of memory rows in the validated backup.
    pub memories: Option<u64>,
    /// Number of embedding mappings in the validated backup.
    pub embeddings: Option<u64>,
    /// Size of the self-contained validated backup in bytes.
    pub bytes: Option<u64>,
    /// Human-readable outcome with remediation where applicable.
    pub message: String,
    /// Process exit code; excluded from JSON because it is communicated by the process.
    #[serde(skip)]
    pub exit_code: i32,
}

impl MaintenanceReport {
    /// Build a stable failure report when configuration prevents an operation from starting.
    #[must_use]
    pub fn configuration_failure<M: Into<String>>(operation: &'static str, requested_path: &Path, message: M) -> Self {
        let configured = Path::new("<configured SQLite database>");
        let (source, destination) = if operation == "backup" {
            (configured, requested_path)
        } else {
            (requested_path, configured)
        };
        failure_report(operation, source, destination, false, MaintenanceFailure::failed(message))
    }

    /// Serialize the stable JSON report with a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns a serialization error only if the report contract itself cannot be encoded.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self).map(|mut json| {
            json.push('\n');
            json
        })
    }

    /// Render a concise operator-facing report.
    #[must_use]
    pub fn render_text(&self) -> String {
        let mut output = format!("{} {}: {}\n", self.operation, self.status, self.message);
        if let Some(recovery_path) = &self.recovery_path {
            let _write_failed = writeln!(output, "Recovery snapshot: {recovery_path}").is_err();
        }
        output
    }
}

#[derive(Debug)]
struct Inspection {
    profile: Option<EmbeddingProfile>,
    memories: u64,
    embeddings: u64,
    bytes: u64,
}

#[derive(Debug)]
struct MaintenanceFailure {
    status: &'static str,
    message: String,
    recovery_path: Option<PathBuf>,
}

impl MaintenanceFailure {
    fn failed(message: impl Into<String>) -> Self {
        Self {
            status: "failed",
            message: message.into(),
            recovery_path: None,
        }
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            status: "invalid",
            message: message.into(),
            recovery_path: None,
        }
    }

    fn with_recovery(mut self, recovery_path: &Path) -> Self {
        self.recovery_path = Some(recovery_path.to_path_buf());
        self
    }
}

/// Create a WAL-consistent, self-contained SQLite backup without overwriting an existing path.
pub async fn backup(options: BackupOptions) -> MaintenanceReport {
    let failure_options = options.clone();
    match tokio::task::spawn_blocking(move || backup_sync(&options)).await {
        Ok(Ok(report)) => report,
        Ok(Err(failure)) => failure_report("backup", &failure_options.database_path, &failure_options.destination, false, failure),
        Err(error) => failure_report(
            "backup",
            &failure_options.database_path,
            &failure_options.destination,
            false,
            MaintenanceFailure::failed(format!("backup worker failed: {error}")),
        ),
    }
}

/// Validate or restore a self-contained SQLite backup.
pub async fn restore(options: RestoreOptions) -> MaintenanceReport {
    if !options.dry_run && !options.confirmed {
        return failure_report("restore", &options.backup_path, &options.database_path, false, MaintenanceFailure {
            status: "refused",
            message: "restore replaces the configured SQLite database; rerun with --yes or use --dry-run".into(),
            recovery_path: None,
        });
    }
    let failure_options = options.clone();
    match tokio::task::spawn_blocking(move || restore_sync(&options)).await {
        Ok(Ok(report)) => report,
        Ok(Err(failure)) => failure_report("restore", &failure_options.backup_path, &failure_options.database_path, failure_options.dry_run, failure),
        Err(error) => failure_report(
            "restore",
            &failure_options.backup_path,
            &failure_options.database_path,
            failure_options.dry_run,
            MaintenanceFailure::failed(format!("restore worker failed: {error}")),
        ),
    }
}

fn backup_sync(options: &BackupOptions) -> Result<MaintenanceReport, MaintenanceFailure> {
    backup_sync_with_policy(options, CopyPolicy::default())
}

fn backup_sync_with_policy(options: &BackupOptions, copy_policy: CopyPolicy) -> Result<MaintenanceReport, MaintenanceFailure> {
    ensure_distinct_paths(&options.database_path, &options.destination)?;
    ensure_new_destination(&options.destination)?;
    let _lease = SqliteDatabaseLease::shared(&options.database_path).map_err(|error| store_failure(&error))?;
    let temporary = temporary_path(&options.destination, "backup");
    let result = (|| {
        snapshot_path(&options.database_path, &temporary, options.clock.as_ref(), copy_policy)?;
        let inspection = inspect_path_using_stored_dimensions(&temporary)?;
        publish_no_clobber(&temporary, &options.destination)?;
        Ok(success_report(
            SuccessReport {
                operation: "backup",
                source: &options.database_path,
                destination: &options.destination,
                dry_run: false,
                database_replaced: false,
                recovery_path: None,
                message: "consistent online backup created and validated".into(),
            },
            inspection,
        ))
    })();
    remove_if_exists(&temporary);
    result
}

fn restore_sync(options: &RestoreOptions) -> Result<MaintenanceReport, MaintenanceFailure> {
    restore_sync_with_replace_policy(options, CopyPolicy::default())
}

#[expect(
    clippy::too_many_lines,
    reason = "restore keeps validation, coordination, recovery creation, transactional replacement, and rollback in one auditable control flow"
)]
fn restore_sync_with_replace_policy(options: &RestoreOptions, replace_policy: CopyPolicy) -> Result<MaintenanceReport, MaintenanceFailure> {
    ensure_distinct_paths(&options.backup_path, &options.database_path)?;
    let staged = temporary_path(&options.database_path, "restore-stage");
    let result = (|| {
        snapshot_path(&options.backup_path, &staged, options.clock.as_ref(), CopyPolicy::default())?;
        let inspection = inspect_path(&staged, options.embedding_dimensions, options.expected_profile.as_ref())?;
        let _lease = match SqliteDatabaseLease::try_exclusive(&options.database_path) {
            Ok(lease) => lease,
            Err(ExclusiveLeaseError::InUse) => {
                return Err(MaintenanceFailure {
                    status: "blocked",
                    message: "the configured SQLite database is open by another LocalHold process; stop every server and retry".into(),
                    recovery_path: None,
                });
            }
            Err(ExclusiveLeaseError::Store(error)) => return Err(store_failure(&error)),
        };

        if options.dry_run {
            return Ok(success_report(
                SuccessReport {
                    operation: "restore",
                    source: &options.backup_path,
                    destination: &options.database_path,
                    dry_run: true,
                    database_replaced: false,
                    recovery_path: None,
                    message: "backup is current, internally consistent, profile-compatible, and the database is quiesced".into(),
                },
                inspection,
            ));
        }

        let target_existed = options.database_path.exists();
        let recovery_path = if target_existed {
            let recovery = recovery_path(&options.database_path, options.clock.as_ref());
            let recovery_temporary = temporary_path(&recovery, "recovery");
            let recovery_result = (|| {
                snapshot_path(&options.database_path, &recovery_temporary, options.clock.as_ref(), CopyPolicy::default())?;
                let _validated = inspect_path_using_stored_dimensions(&recovery_temporary)?;
                publish_no_clobber(&recovery_temporary, &recovery)
            })();
            remove_if_exists(&recovery_temporary);
            recovery_result?;
            Some(recovery)
        } else {
            None
        };

        if !target_existed {
            create_secure_empty_file(&options.database_path)?;
        }
        let replace_result = replace_from_snapshot(&staged, &options.database_path, options.clock.as_ref(), replace_policy);
        if let Err(failure) = replace_result {
            if !target_existed {
                remove_sqlite_files(&options.database_path);
            }
            return Err(if let Some(recovery) = &recovery_path {
                failure.with_recovery(recovery)
            } else {
                failure
            });
        }

        if let Err(validation_failure) = inspect_path(&options.database_path, options.embedding_dimensions, options.expected_profile.as_ref()) {
            if let Some(recovery) = &recovery_path {
                rollback_invalid_restore(options, recovery, &validation_failure)?;
            } else {
                remove_sqlite_files(&options.database_path);
            }
            let failure = MaintenanceFailure::failed(format!(
                "restored database failed post-write validation and the previous database was recovered: {}",
                validation_failure.message
            ));
            return Err(if let Some(recovery) = &recovery_path {
                failure.with_recovery(recovery)
            } else {
                failure
            });
        }

        Ok(success_report(
            SuccessReport {
                operation: "restore",
                source: &options.backup_path,
                destination: &options.database_path,
                dry_run: false,
                database_replaced: true,
                recovery_path: recovery_path.as_deref(),
                message: "validated backup restored transactionally; the pre-restore snapshot was retained".into(),
            },
            inspection,
        ))
    })();
    remove_if_exists(&staged);
    result
}

fn rollback_invalid_restore(options: &RestoreOptions, recovery: &Path, validation_failure: &MaintenanceFailure) -> Result<(), MaintenanceFailure> {
    replace_from_snapshot(recovery, &options.database_path, options.clock.as_ref(), CopyPolicy::default()).map_err(|rollback_failure| {
        MaintenanceFailure::failed(format!(
            "restored database failed post-write validation ({}) and automatic rollback also failed ({}); recovery snapshot remains at {}",
            validation_failure.message,
            rollback_failure.message,
            recovery.display()
        ))
        .with_recovery(recovery)
    })
}

fn snapshot_path(source_path: &Path, destination_path: &Path, clock: &dyn Clock, policy: CopyPolicy) -> Result<(), MaintenanceFailure> {
    let metadata = source_path
        .metadata()
        .map_err(|error| MaintenanceFailure::failed(format!("cannot inspect {}: {error}", source_path.display())))?;
    if !metadata.is_file() {
        return Err(MaintenanceFailure::invalid(format!("{} is not a regular database file", source_path.display())));
    }
    SqliteStore::register_extension().map_err(|error| store_failure(&error))?;
    let source = Connection::open_with_flags(source_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sqlite_failure("open backup source"))?;
    source.busy_timeout(Duration::ZERO).map_err(sqlite_failure("configure backup source"))?;
    create_secure_empty_file(destination_path)?;
    let mut destination = Connection::open(destination_path).map_err(sqlite_failure("open backup destination"))?;
    destination.busy_timeout(Duration::ZERO).map_err(sqlite_failure("configure backup destination"))?;
    copy_database(&source, &mut destination, clock, &tokio::runtime::Handle::current(), policy)?;
    drop(destination);
    sync_file(destination_path)?;
    Ok(())
}

fn replace_from_snapshot(source_path: &Path, destination_path: &Path, clock: &dyn Clock, policy: CopyPolicy) -> Result<(), MaintenanceFailure> {
    SqliteStore::register_extension().map_err(|error| store_failure(&error))?;
    let source = Connection::open_with_flags(source_path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sqlite_failure("open restore source"))?;
    source.busy_timeout(Duration::ZERO).map_err(sqlite_failure("configure restore source"))?;
    let mut destination = Connection::open(destination_path).map_err(sqlite_failure("open restore destination"))?;
    destination.busy_timeout(Duration::ZERO).map_err(sqlite_failure("configure restore destination"))?;
    copy_database(&source, &mut destination, clock, &tokio::runtime::Handle::current(), policy)?;
    drop(destination);
    sync_file(destination_path)?;
    #[cfg(unix)]
    sync_parent(destination_path)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Default)]
struct CopyPolicy {
    abort_after_steps_for_test: Option<usize>,
    disk_full_after_steps_for_test: Option<usize>,
}

fn copy_database(source: &Connection, destination: &mut Connection, clock: &dyn Clock, runtime: &tokio::runtime::Handle, policy: CopyPolicy) -> Result<(), MaintenanceFailure> {
    let backup = rusqlite::backup::Backup::new(source, destination).map_err(sqlite_failure("initialize online backup"))?;
    let started = clock.monotonic();
    let mut steps = 0_usize;
    loop {
        let outcome = backup.step(COPY_PAGES_PER_STEP).map_err(sqlite_failure("copy database pages"))?;
        steps = steps.saturating_add(1);
        if policy.abort_after_steps_for_test.is_some_and(|limit| steps >= limit) && outcome != StepResult::Done {
            return Err(MaintenanceFailure::failed("injected interrupted restore"));
        }
        if policy.disk_full_after_steps_for_test.is_some_and(|limit| steps >= limit) && outcome != StepResult::Done {
            return Err(MaintenanceFailure::failed("copy database pages: database or disk is full"));
        }
        match outcome {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            StepResult::Busy | StepResult::Locked => {
                if clock.monotonic().saturating_sub(started) >= COPY_TIMEOUT {
                    return Err(MaintenanceFailure {
                        status: "blocked",
                        message: "SQLite remained busy for 30 seconds while copying; stop competing writers and retry".into(),
                        recovery_path: None,
                    });
                }
                runtime.block_on(clock.sleep(COPY_RETRY_DELAY));
            }
            _ => return Err(MaintenanceFailure::failed("SQLite returned an unsupported online-backup result")),
        }
    }
}

fn inspect_path(path: &Path, embedding_dimensions: usize, expected_profile: Option<&EmbeddingProfile>) -> Result<Inspection, MaintenanceFailure> {
    SqliteStore::register_extension().map_err(|error| store_failure(&error))?;
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sqlite_failure("open database for validation"))?;
    connection.pragma_update(None, "query_only", true).map_err(sqlite_failure("enable read-only validation"))?;
    connection
        .pragma_update(None, "foreign_keys", true)
        .map_err(sqlite_failure("enable foreign-key validation"))?;
    validate_integrity(&connection)?;
    validate_sqlite_source_schema(&connection, embedding_dimensions).map_err(|error| MaintenanceFailure::invalid(error.to_string()))?;
    let profile = read_embedding_profile(&connection).map_err(|error| store_failure(&error))?;
    if let (Some(expected), Some(stored)) = (expected_profile, profile.as_ref())
        && expected != stored
    {
        return Err(MaintenanceFailure::invalid(format!(
            "embedding profile mismatch: backup uses {} model '{}' at '{}' with {} dimensions, but config selects {} model '{}' at '{}' with {} dimensions",
            stored.provider, stored.model, stored.endpoint, stored.dimensions, expected.provider, expected.model, expected.endpoint, expected.dimensions
        )));
    }
    let memories = count_rows(&connection, "memories")?;
    let embeddings = count_rows(&connection, "memory_embedding_map")?;
    if profile.is_none() && embeddings > 0 {
        return Err(MaintenanceFailure::invalid(
            "backup contains embeddings but no recorded embedding profile; reindex the source before backing it up",
        ));
    }
    let bytes = path
        .metadata()
        .map_err(|error| MaintenanceFailure::failed(format!("cannot stat validated backup {}: {error}", path.display())))?
        .len();
    Ok(Inspection {
        profile,
        memories,
        embeddings,
        bytes,
    })
}

fn inspect_path_using_stored_dimensions(path: &Path) -> Result<Inspection, MaintenanceFailure> {
    SqliteStore::register_extension().map_err(|error| store_failure(&error))?;
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sqlite_failure("open database dimension metadata"))?;
    let dimensions = existing_embedding_dimensions(&connection)
        .map_err(|error| store_failure(&error))?
        .ok_or_else(|| MaintenanceFailure::invalid("database is missing the managed vector table dimension metadata"))?;
    drop(connection);
    inspect_path(path, dimensions, None)
}

fn count_rows(connection: &Connection, table: &'static str) -> Result<u64, MaintenanceFailure> {
    let count: i64 = connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
        .map_err(sqlite_failure("count database rows"))?;
    u64::try_from(count).map_err(|_error| MaintenanceFailure::invalid(format!("table {table} returned a negative row count")))
}

fn validate_integrity(connection: &Connection) -> Result<(), MaintenanceFailure> {
    let mut statement = connection.prepare("PRAGMA integrity_check").map_err(sqlite_failure("prepare integrity check"))?;
    let messages = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sqlite_failure("run integrity check"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_failure("read integrity check"))?;
    if messages.as_slice() != ["ok"] {
        return Err(MaintenanceFailure::invalid(format!("SQLite integrity_check failed: {}", messages.join("; "))));
    }
    Ok(())
}

fn ensure_distinct_paths(first: &Path, second: &Path) -> Result<(), MaintenanceFailure> {
    let first = first
        .canonicalize()
        .map_err(|error| MaintenanceFailure::failed(format!("cannot resolve {}: {error}", first.display())))?;
    let second_resolved = if second.exists() {
        second
            .canonicalize()
            .map_err(|error| MaintenanceFailure::failed(format!("cannot resolve {}: {error}", second.display())))?
    } else {
        let parent = second.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
        let parent = parent
            .canonicalize()
            .map_err(|error| MaintenanceFailure::failed(format!("cannot resolve destination directory {}: {error}", parent.display())))?;
        parent.join(second.file_name().ok_or_else(|| MaintenanceFailure::invalid("destination must name a file"))?)
    };
    if first == second_resolved {
        return Err(MaintenanceFailure::invalid("source and destination resolve to the same file"));
    }
    Ok(())
}

fn ensure_new_destination(destination: &Path) -> Result<(), MaintenanceFailure> {
    match destination.symlink_metadata() {
        Ok(_) => Err(MaintenanceFailure {
            status: "refused",
            message: format!("destination {} already exists; choose a new backup path", destination.display()),
            recovery_path: None,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(MaintenanceFailure::failed(format!("cannot inspect destination {}: {error}", destination.display()))),
    }
}

fn create_secure_empty_file(path: &Path) -> Result<(), MaintenanceFailure> {
    let mut options = OpenOptions::new();
    let _configured = options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let _mode = options.mode(0o600);
    }
    options
        .open(path)
        .map(drop)
        .map_err(|error| MaintenanceFailure::failed(format!("cannot create {}: {error}", path.display())))
}

fn publish_no_clobber(temporary: &Path, destination: &Path) -> Result<(), MaintenanceFailure> {
    std::fs::hard_link(temporary, destination).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            MaintenanceFailure {
                status: "refused",
                message: format!("destination {} appeared during the operation and was not overwritten", destination.display()),
                recovery_path: None,
            }
        } else {
            MaintenanceFailure::failed(format!("cannot publish {}: {error}", destination.display()))
        }
    })?;
    std::fs::remove_file(temporary).map_err(|error| MaintenanceFailure::failed(format!("backup was published but temporary cleanup failed: {error}")))?;
    #[cfg(unix)]
    sync_parent(destination)?;
    Ok(())
}

fn sync_file(path: &Path) -> Result<(), MaintenanceFailure> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| MaintenanceFailure::failed(format!("cannot durably sync {}: {error}", path.display())))
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), MaintenanceFailure> {
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| MaintenanceFailure::failed(format!("cannot durably sync directory {}: {error}", parent.display())))
}

fn temporary_path(base: &Path, label: &str) -> PathBuf {
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut path = OsString::from(base.as_os_str());
    path.push(format!(".localhold-{label}-{}-{sequence}", std::process::id()));
    PathBuf::from(path)
}

fn recovery_path(database: &Path, clock: &dyn Clock) -> PathBuf {
    let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = clock.now().format("%Y%m%dT%H%M%SZ");
    let mut path = OsString::from(database.as_os_str());
    path.push(format!(".pre-restore-{timestamp}-{sequence}.bak"));
    PathBuf::from(path)
}

fn remove_if_exists(path: &Path) {
    if let Err(error) = std::fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %path.display(), %error, "failed to remove SQLite maintenance temporary file");
    }
}

fn remove_sqlite_files(database: &Path) {
    remove_if_exists(database);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = OsString::from(database.as_os_str());
        sidecar.push(suffix);
        remove_if_exists(Path::new(&sidecar));
    }
}

struct SuccessReport<'a> {
    operation: &'static str,
    source: &'a Path,
    destination: &'a Path,
    dry_run: bool,
    database_replaced: bool,
    recovery_path: Option<&'a Path>,
    message: String,
}

fn success_report(details: SuccessReport<'_>, inspection: Inspection) -> MaintenanceReport {
    MaintenanceReport {
        schema_version: REPORT_SCHEMA_VERSION,
        operation: details.operation,
        status: if details.dry_run { "validated" } else { "ok" },
        source: details.source.to_string_lossy().into_owned(),
        destination: details.destination.to_string_lossy().into_owned(),
        dry_run: details.dry_run,
        database_replaced: details.database_replaced,
        recovery_path: details.recovery_path.map(|path| path.to_string_lossy().into_owned()),
        database_schema_version: Some(SQLITE_SCHEMA_VERSION),
        embedding_profile: inspection.profile,
        memories: Some(inspection.memories),
        embeddings: Some(inspection.embeddings),
        bytes: Some(inspection.bytes),
        message: details.message,
        exit_code: 0,
    }
}

fn failure_report(operation: &'static str, source: &Path, destination: &Path, dry_run: bool, failure: MaintenanceFailure) -> MaintenanceReport {
    MaintenanceReport {
        schema_version: REPORT_SCHEMA_VERSION,
        operation,
        status: failure.status,
        source: source.to_string_lossy().into_owned(),
        destination: destination.to_string_lossy().into_owned(),
        dry_run,
        database_replaced: false,
        recovery_path: failure.recovery_path.map(|path| path.to_string_lossy().into_owned()),
        database_schema_version: None,
        embedding_profile: None,
        memories: None,
        embeddings: None,
        bytes: None,
        message: failure.message,
        exit_code: 1,
    }
}

fn store_failure(error: &StoreError) -> MaintenanceFailure {
    MaintenanceFailure::failed(error.to_string())
}

fn sqlite_failure(context: &'static str) -> impl FnOnce(rusqlite::Error) -> MaintenanceFailure {
    move |error| MaintenanceFailure::failed(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{TimeZone as _, Utc};

    use super::*;
    use crate::{
        clock::MockClock,
        store::{MemoryAdmin as _, MemoryWriter as _},
        types::{AccessPolicy, AuditAction, AuditDraft, Entity, Memory, MemoryMetadata, Provenance, ScopeDefinition, WriteOutcome},
    };

    const DIMENSIONS: usize = 3;

    fn profile(model: &str) -> EmbeddingProfile {
        EmbeddingProfile::openai_compatible("http://127.0.0.1:8000/v1", model, DIMENSIONS)
    }

    #[test]
    fn recovery_names_use_the_injected_clock() {
        let clock = MockClock::pinned(Utc.with_ymd_and_hms(2031, 2, 3, 4, 5, 6).unwrap());
        let path = recovery_path(Path::new("memory.db"), &clock);
        assert!(path.to_string_lossy().contains("20310203T040506Z"));
    }

    async fn seed_database(path: &Path, label: &str, profile: &EmbeddingProfile) -> SqliteStore {
        let store = SqliteStore::open(path, DIMENSIONS).unwrap();
        store.verify_embedding_profile(profile).await.unwrap();

        let provenance = Provenance::new_for_test(Some("owner".into()), Some("scope/test".into()), Some("origin/test".into()));
        let mut memory = Memory::new_for_test(format!("{label} live memory"), vec!["backup".into()], provenance.clone(), AccessPolicy::Public);
        memory.entities.push(Entity::new("LocalHold", "project").unwrap());
        let audit = AuditDraft {
            action: AuditAction::Store,
            caller_agent: Some("owner".into()),
            timestamp: Utc.with_ymd_and_hms(2026, 7, 14, 12, 0, 0).unwrap(),
            details: Some(serde_json::json!({"fixture": label})),
        };
        let memory_id = store.store_audited(&memory, Some(&[0.1_f32, 0.2_f32, 0.3_f32]), &audit).await.unwrap();
        store
            .upsert_metadata(MemoryMetadata {
                memory_id,
                scope_key: Some("scope/test".into()),
                summary: Some(format!("{label} summary")),
                agent_label: Some("fixture".into()),
                created_by_principal: Some("owner".into()),
                quality_flags: vec!["fixture".into()],
                schema_version: 1,
            })
            .await
            .unwrap();
        store
            .register_scope(ScopeDefinition {
                scope_key: "scope/test".into(),
                display_name: "Test scope".into(),
                description: Some("backup fixture".into()),
                aliases: vec!["fixture".into()],
                matchers: vec!["/tmp/fixture".into()],
                parent: None,
                related: Vec::new(),
            })
            .await
            .unwrap();

        let deleted = Memory::new_for_test(format!("{label} deleted memory"), Vec::new(), provenance, AccessPolicy::Public);
        let deleted_id = store.store(&deleted, None).await.unwrap();
        let delete_audit = AuditDraft {
            action: AuditAction::Delete,
            caller_agent: Some("owner".into()),
            timestamp: Utc.with_ymd_and_hms(2026, 7, 14, 12, 1, 0).unwrap(),
            details: None,
        };
        assert_eq!(store.delete_authorized_audited(&deleted_id, "owner", &delete_audit).await.unwrap(), WriteOutcome::Applied);
        store
    }

    fn managed_counts(path: &Path) -> BTreeMap<String, i64> {
        SqliteStore::register_extension().unwrap();
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        [
            "memories",
            "memory_embedding_map",
            "memory_embeddings",
            "memory_entities",
            "memory_fts",
            "memory_audit_log",
            "memory_tombstones",
            "scope_registry",
            "memory_metadata",
            "embedding_profile",
        ]
        .into_iter()
        .map(|table| {
            let count = connection.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0)).unwrap();
            (table.to_owned(), count)
        })
        .collect()
    }

    fn memory_contents(path: &Path) -> Vec<String> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let mut statement = connection.prepare("SELECT content FROM memories ORDER BY content").unwrap();
        statement.query_map([], |row| row.get(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap()
    }

    async fn add_large_fixture_rows(store: &SqliteStore, id_prefix: &'static str, count: usize, payload: String) {
        store
            .with_conn(move |connection| {
                for index in 0_usize..count {
                    let id = format!("{id_prefix}{index:06}");
                    let _inserted = connection.execute(
                        "INSERT INTO memories (id, content, tags, provenance, access_policy, created_at, updated_at) VALUES (?1, ?2, '[]', '{}', '{\"type\":\"public\"}', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                        (&id, &payload),
                    )?;
                }
                Ok(())
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn backup_and_restore_round_trip_every_managed_schema_surface() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let backup_path = directory.path().join("snapshot.db");
        let target = directory.path().join("target.db");
        let expected = profile("model-a");
        let source_store = seed_database(&source, "source", &expected).await;
        let source_counts = managed_counts(&source);

        let backup_report = backup(BackupOptions::new(source.clone(), backup_path.clone())).await;
        assert_eq!(backup_report.status, "ok");
        assert_eq!(backup_report.memories, Some(1));
        assert_eq!(backup_report.embeddings, Some(1));
        assert_eq!(backup_report.embedding_profile, Some(expected.clone()));
        assert_eq!(managed_counts(&backup_path), source_counts);
        assert!(
            source.with_extension("db-wal").exists() || source.with_extension("db-shm").exists(),
            "source should remain open in WAL mode"
        );
        drop(source_store);

        let target_store = seed_database(&target, "target", &expected).await;
        drop(target_store);
        let dry_run = restore(
            RestoreOptions::new(target.clone(), backup_path.clone(), DIMENSIONS, Some(expected.clone()))
                .dry_run(true)
                .confirmed(false),
        )
        .await;
        assert_eq!(dry_run.status, "validated");
        assert!(!dry_run.database_replaced);
        assert!(memory_contents(&target).iter().any(|content| content.contains("target")));

        let restored = restore(RestoreOptions::new(target.clone(), backup_path, DIMENSIONS, Some(expected)).confirmed(true)).await;
        assert_eq!(restored.status, "ok");
        assert!(restored.database_replaced);
        let recovery = restored.recovery_path.as_ref().map(PathBuf::from).unwrap();
        assert!(recovery.exists());
        assert_eq!(managed_counts(&target), source_counts);
        assert!(memory_contents(&target).iter().any(|content| content.contains("source")));
        assert!(memory_contents(&recovery).iter().any(|content| content.contains("target")));
    }

    #[tokio::test]
    async fn backup_is_consistent_while_a_wal_writer_is_active() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let backup_path = directory.path().join("snapshot.db");
        let expected = profile("model-a");
        let source_store = seed_database(&source, "committed", &expected).await;
        let writer_path = source.clone();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel::<()>(0);
        let (finish_tx, finish_rx) = std::sync::mpsc::sync_channel::<()>(0);
        let writer = std::thread::spawn(move || {
            let connection = Connection::open(writer_path).unwrap();
            connection.execute_batch("BEGIN IMMEDIATE").unwrap();
            let _inserted = connection
                .execute(
                    "INSERT INTO memories (id, content, tags, provenance, access_policy, created_at, updated_at) VALUES ('01J20000000000000000000000', 'uncommitted writer', '[]', '{}', '{\"type\":\"public\"}', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                    [],
                )
                .unwrap();
            started_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
            connection.execute_batch("COMMIT").unwrap();
        });
        started_rx.recv().unwrap();

        let report = backup(BackupOptions::new(source.clone(), backup_path.clone())).await;
        assert_eq!(report.status, "ok");
        assert_eq!(report.memories, Some(1), "an online backup must exclude the writer's uncommitted row");
        assert_eq!(memory_contents(&backup_path), vec!["committed live memory"]);
        finish_tx.send(()).unwrap();
        writer.join().unwrap();
        assert_eq!(memory_contents(&source).len(), 2_usize, "writer should commit after the consistent snapshot completes");
        drop(source_store);
    }

    #[tokio::test]
    async fn restore_is_blocked_while_any_store_holds_the_database_open() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let backup_path = directory.path().join("snapshot.db");
        let target = directory.path().join("target.db");
        let expected = profile("model-a");
        let source_store = seed_database(&source, "source", &expected).await;
        assert_eq!(backup(BackupOptions::new(source, backup_path.clone())).await.status, "ok");
        drop(source_store);
        let target_store = seed_database(&target, "target", &expected).await;

        let report = restore(RestoreOptions::new(target, backup_path, DIMENSIONS, Some(expected)).confirmed(true)).await;
        assert_eq!(report.status, "blocked");
        assert!(report.message.contains("another LocalHold process"));
        drop(target_store);
    }

    #[tokio::test]
    async fn backup_never_clobbers_an_existing_destination() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let destination = directory.path().join("existing.db");
        let expected = profile("model-a");
        let _source_store = seed_database(&source, "source", &expected).await;
        std::fs::write(&destination, b"do not replace").unwrap();

        let report = backup(BackupOptions::new(source, destination.clone())).await;
        assert_eq!(report.status, "refused");
        assert_eq!(std::fs::read(destination).unwrap(), b"do not replace");
    }

    #[tokio::test]
    async fn restore_rejects_corruption_schema_and_embedding_profile_before_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let backup_path = directory.path().join("snapshot.db");
        let target = directory.path().join("target.db");
        let corrupt = directory.path().join("corrupt.db");
        let wrong_schema = directory.path().join("wrong-schema.db");
        let expected = profile("model-a");
        let source_store = seed_database(&source, "source", &expected).await;
        assert_eq!(backup(BackupOptions::new(source, backup_path.clone())).await.status, "ok");
        drop(source_store);
        let target_store = seed_database(&target, "target", &expected).await;
        drop(target_store);
        let original = memory_contents(&target);

        std::fs::write(&corrupt, b"not sqlite").unwrap();
        let corrupt_report = restore(RestoreOptions::new(target.clone(), corrupt, DIMENSIONS, Some(expected.clone())).dry_run(true)).await;
        assert!(matches!(corrupt_report.status, "failed" | "invalid"));
        assert_eq!(memory_contents(&target), original);

        let _bytes_copied = std::fs::copy(&backup_path, &wrong_schema).unwrap();
        let schema_connection = Connection::open(&wrong_schema).unwrap();
        schema_connection.pragma_update(None, "user_version", 99_u32).unwrap();
        drop(schema_connection);
        let schema_report = restore(RestoreOptions::new(target.clone(), wrong_schema, DIMENSIONS, Some(expected.clone())).dry_run(true)).await;
        assert_eq!(schema_report.status, "invalid");
        assert!(schema_report.message.contains("schema version"));
        assert_eq!(memory_contents(&target), original);

        let profile_report = restore(RestoreOptions::new(target.clone(), backup_path, DIMENSIONS, Some(profile("model-b"))).dry_run(true)).await;
        assert_eq!(profile_report.status, "invalid");
        assert!(profile_report.message.contains("embedding profile mismatch"));
        assert_eq!(memory_contents(&target), original);
    }

    #[tokio::test]
    async fn interrupted_restore_rolls_back_destination_and_reports_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let backup_path = directory.path().join("snapshot.db");
        let target = directory.path().join("target.db");
        let expected = profile("model-a");
        let source_store = seed_database(&source, "source", &expected).await;
        add_large_fixture_rows(&source_store, "01J00000000000000000", 600_usize, "x".repeat(4096)).await;
        assert_eq!(backup(BackupOptions::new(source, backup_path.clone())).await.status, "ok");
        drop(source_store);
        let target_store = seed_database(&target, "target", &expected).await;
        drop(target_store);
        let original = memory_contents(&target);
        let options = RestoreOptions::new(target.clone(), backup_path, DIMENSIONS, Some(expected)).confirmed(true);

        let failure = tokio::task::spawn_blocking(move || {
            restore_sync_with_replace_policy(&options, CopyPolicy {
                abort_after_steps_for_test: Some(1),
                ..CopyPolicy::default()
            })
        })
        .await
        .unwrap()
        .unwrap_err();
        assert!(failure.message.contains("interrupted"));
        let recovery = failure.recovery_path.unwrap();
        assert!(recovery.exists());
        assert_eq!(memory_contents(&target), original, "SQLite backup transaction must roll back an interrupted restore");
        assert_eq!(memory_contents(&recovery), original);
    }

    #[tokio::test]
    async fn interrupted_backup_never_publishes_or_leaves_partial_files() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let backup_path = directory.path().join("snapshot.db");
        let expected = profile("model-a");
        let source_store = seed_database(&source, "source", &expected).await;
        add_large_fixture_rows(&source_store, "01J30000000000000000", 600_usize, "partial".repeat(1024)).await;
        let options = BackupOptions::new(source, backup_path.clone());

        let failure = tokio::task::spawn_blocking(move || {
            backup_sync_with_policy(&options, CopyPolicy {
                abort_after_steps_for_test: Some(1),
                ..CopyPolicy::default()
            })
        })
        .await
        .unwrap()
        .unwrap_err();
        assert!(failure.message.contains("interrupted"));
        assert!(!backup_path.exists());
        let partials = std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("localhold-backup"))
            .count();
        assert_eq!(partials, 0_usize, "failed backups must clean private staging files");
        drop(source_store);
    }

    #[tokio::test]
    async fn sqlite_full_rolls_back_destination_without_losing_current_data() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let backup_path = directory.path().join("snapshot.db");
        let target = directory.path().join("target.db");
        let expected = profile("model-a");
        let source_store = seed_database(&source, "source", &expected).await;
        add_large_fixture_rows(&source_store, "01J10000000000000000", 300_usize, "full".repeat(2048)).await;
        assert_eq!(backup(BackupOptions::new(source, backup_path.clone())).await.status, "ok");
        drop(source_store);

        let target_store = seed_database(&target, "target", &expected).await;
        drop(target_store);
        let original = memory_contents(&target);
        let options = RestoreOptions::new(target.clone(), backup_path, DIMENSIONS, Some(expected)).confirmed(true);
        let failure = tokio::task::spawn_blocking(move || {
            restore_sync_with_replace_policy(&options, CopyPolicy {
                disk_full_after_steps_for_test: Some(1),
                ..CopyPolicy::default()
            })
        })
        .await
        .unwrap()
        .unwrap_err();
        assert!(failure.message.contains("disk is full"));
        assert!(failure.recovery_path.as_ref().is_some_and(|path| path.exists()));
        assert_eq!(memory_contents(&target), original, "SQLITE_FULL must roll back the destination transaction");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn backup_is_private_and_restore_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.db");
        let backup_path = directory.path().join("snapshot.db");
        let target = directory.path().join("target.db");
        let expected = profile("model-a");
        let source_store = seed_database(&source, "source", &expected).await;
        assert_eq!(backup(BackupOptions::new(source, backup_path.clone())).await.status, "ok");
        drop(source_store);
        assert_eq!(backup_path.metadata().unwrap().permissions().mode() & 0o777, 0o600);

        let target_store = seed_database(&target, "target", &expected).await;
        drop(target_store);
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(
            restore(RestoreOptions::new(target.clone(), backup_path, DIMENSIONS, Some(expected)).confirmed(true))
                .await
                .status,
            "ok"
        );
        assert_eq!(target.metadata().unwrap().permissions().mode() & 0o777, 0o640);
    }

    #[tokio::test]
    async fn report_json_has_stable_contract_and_excludes_process_exit_code() {
        let report = MaintenanceReport {
            schema_version: REPORT_SCHEMA_VERSION,
            operation: "backup",
            status: "ok",
            source: "source.db".into(),
            destination: "backup.db".into(),
            dry_run: false,
            database_replaced: false,
            recovery_path: None,
            database_schema_version: Some(SQLITE_SCHEMA_VERSION),
            embedding_profile: Some(profile("model-a")),
            memories: Some(1),
            embeddings: Some(1),
            bytes: Some(4096),
            message: "created".into(),
            exit_code: 0,
        };
        let json: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();
        assert_eq!(json["schema_version"], 1_i32);
        assert_eq!(json["database_schema_version"], 1_i32);
        assert_eq!(json["operation"], "backup");
        assert_eq!(json["embedding_profile"]["model"], "model-a");
        assert!(json.get("exit_code").is_none());
    }
}
