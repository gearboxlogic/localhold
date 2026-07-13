use std::{
    io::{BufRead as _, BufReader, Read as _, Write as _},
    net::TcpListener,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use localhold::{
    store::{EmbeddingProfile, MemoryWriter as _, SqliteStore},
    types::{AccessPolicy, Memory, Provenance},
};
use serde_json::Value;
use sqlx_core::{query::query, query_scalar::query_scalar};
use sqlx_postgres::PgPoolOptions;

fn unique_db_path(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("localhold-{name}-{}-{nanos}.db", std::process::id()))
}

fn base_binary_command(db_path: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hold"));
    let _isolated_config = isolate_user_config_dir(&mut cmd, &db_path.with_extension("base-config"));
    cmd.env_remove("LOCALHOLD_DB_BACKEND");
    cmd.env_remove("LOCALHOLD_POSTGRES_URL");
    cmd.env("LOCALHOLD_DB_PATH", db_path);
    cmd.env("LOCALHOLD_LOG_LEVEL", "info");
    cmd
}

fn isolate_user_config_dir(command: &mut Command, root: &std::path::Path) -> std::path::PathBuf {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let config_dir = root.join("user-config");
        command.env("XDG_CONFIG_HOME", &config_dir);
        config_dir
    }
    #[cfg(target_os = "macos")]
    {
        command.env("HOME", root);
        root.join("Library/Application Support")
    }
    #[cfg(windows)]
    {
        let config_dir = root.join("AppData/Roaming");
        command.env("APPDATA", &config_dir);
        config_dir
    }
}

#[expect(clippy::expect_used, clippy::panic, reason = "test helper reports subprocess startup failures with captured diagnostics")]
fn wait_for_startup_log(child: &mut Child, label: &str, expected: &str) -> JoinHandle<String> {
    let stderr = child.stderr.take().expect("startup-log test must pipe stderr");
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut output = String::new();
        for line in BufReader::new(stderr).lines() {
            let line = line.expect("child stderr should remain valid UTF-8 text");
            output.push_str(&line);
            output.push('\n');
            if sender.send(line).is_err() {
                break;
            }
        }
        output
    });

    loop {
        match receiver.recv_timeout(Duration::from_secs(30)) {
            Ok(line) if line.contains(expected) => return reader,
            Ok(_line) => {}
            Err(error) => {
                let status = child.try_wait().unwrap();
                terminate_child(child);
                let stderr = reader.join().unwrap();
                panic!("{label} did not emit startup readiness log {expected:?}: {error}; status={status:?}; stderr={stderr}");
            }
        }
    }
}

fn terminate_child(child: &mut Child) {
    let _kill = child.kill();
    let _status = child.wait();
}

fn postgres_smoke_url() -> String {
    std::env::var("LOCALHOLD_POSTGRES_URL").unwrap_or_else(|_| "postgres://localhold:localhold@localhost:55432/localhold".into())
}

fn assert_destructive_postgres_smoke_allowed() {
    let allowed = std::env::var("LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE").is_ok_and(|value| value == "1");
    assert!(allowed, "destructive PostgreSQL smoke cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1");
}

fn drop_postgres_smoke_schema(url: &str) {
    assert_destructive_postgres_smoke_allowed();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let pool = PgPoolOptions::new().max_connections(1).connect(url).await.unwrap();
        let _result = query(
            "
            DROP TABLE IF EXISTS
                memory_audit_log,
                memory_tombstones,
                memory_metadata,
                memory_entities,
                memory_embeddings,
                memories,
                scope_registry,
                localhold_migrations
            CASCADE
            ",
        )
        .execute(&pool)
        .await
        .unwrap();
    });
}

fn seed_sqlite_migration_source(path: &std::path::Path) {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let store = SqliteStore::open(path, 3_usize).unwrap();
        let memory = Memory::new_for_test(
            "binary migration smoke memory".into(),
            vec!["binary-migration".into()],
            Provenance::new_for_test(Some("binary-smoke".into()), Some("binary-smoke/source".into()), None),
            AccessPolicy::Public,
        );
        let embedding = [0.1_f32, 0.2_f32, 0.3_f32];
        let _id = store.store(&memory, Some(&embedding)).await.unwrap();
    });
}

fn migrated_memory_count(url: &str) -> i64 {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let pool = PgPoolOptions::new().max_connections(1).connect(url).await.unwrap();
        query_scalar::<_, i64>("SELECT COUNT(*) FROM memories WHERE content = $1")
            .bind("binary migration smoke memory")
            .fetch_one(&pool)
            .await
            .unwrap()
    })
}

#[test]
fn binary_starts_in_stdio_mode() {
    let db_path = unique_db_path("bin-stdio");
    let mut cmd = base_binary_command(&db_path);
    cmd.env("LOCALHOLD_TRANSPORT", "stdio");
    cmd.env("LOCALHOLD_LOG_LEVEL", "info");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().unwrap();
    let stderr = wait_for_startup_log(&mut child, "stdio", "noop embedding provider initialized");
    terminate_child(&mut child);
    let _stderr = stderr.join().unwrap();
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_json_reports_degraded_without_creating_missing_database_or_leaking_secrets() {
    let db_path = unique_db_path("doctor-missing");
    let mut cmd = base_binary_command(&db_path);
    cmd.env("LOCALHOLD_HTTP_AUTH_TOKEN", "doctor-super-secret-token");
    let output = cmd.args(["doctor", "--json"]).output().unwrap();

    assert_eq!(output.status.code(), Some(2_i32), "missing database should be degraded");
    assert!(!db_path.exists(), "doctor must not create a missing database");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["schema_version"], 1_i32);
    assert_eq!(report["status"], "degraded");
    assert_eq!(report["exit_code"], 2_i32);
    assert!(!stdout.contains("doctor-super-secret-token"), "doctor output must redact configured secrets");
}

#[test]
fn doctor_degrades_for_empty_sqlite_file_without_bootstrapping_it() {
    let db_path = unique_db_path("doctor-empty");
    std::fs::File::create(&db_path).unwrap();

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "degraded");
    assert_eq!(std::fs::metadata(&db_path).unwrap().len(), 0_u64, "doctor must not bootstrap the empty file");

    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_reports_healthy_for_current_sqlite_database() {
    let db_path = unique_db_path("doctor-healthy");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();

    let output = base_binary_command(&db_path).arg("doctor").output().unwrap();
    assert!(
        output.status.success(),
        "current SQLite database should be healthy: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("LocalHold doctor: healthy"));
    assert!(stdout.contains("[healthy] storage:"));

    drop(store);
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_degrades_when_existing_sqlite_wal_sidecars_are_absent() {
    let db_path = unique_db_path("doctor-missing-sidecars");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "degraded");
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["name"] == "filesystem" && check["status"] == "degraded")
    );

    let _cleanup = std::fs::remove_file(db_path);
}

#[cfg(unix)]
#[test]
fn doctor_does_not_recreate_missing_shm_for_existing_wal() {
    let db_path = unique_db_path("doctor-wal-without-shm");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    let wal_path = db_path.with_extension("db-wal");
    let shm_path = db_path.with_extension("db-shm");
    assert!(wal_path.exists());
    std::fs::remove_file(&shm_path).unwrap();

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2_i32));
    assert!(!shm_path.exists(), "read-only doctor must not recreate SQLite shared-memory state");

    drop(store);
    let _cleanup = std::fs::remove_file(wal_path);
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_when_missing_sqlite_parent_is_a_file() {
    let parent_path = unique_db_path("doctor-parent-file");
    std::fs::File::create(&parent_path).unwrap();
    let db_path = parent_path.join("localhold.db");

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert!(!db_path.exists());

    let _cleanup = std::fs::remove_file(parent_path);
}

#[test]
fn doctor_fails_when_sqlite_vector_dimensions_do_not_match_config() {
    let db_path = unique_db_path("doctor-dimension-mismatch");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);

    let mut command = base_binary_command(&db_path);
    command.env("LOCALHOLD_EMBEDDING_DIMENSIONS", "384");
    let output = command.args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "failed");

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_degrades_when_sqlite_schema_needs_audit_log_migration() {
    let db_path = unique_db_path("doctor-audit-migration");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.execute("DROP TABLE memory_audit_log", []).unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "degraded");

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_dimension_mismatch_even_when_sqlite_migration_is_pending() {
    let db_path = unique_db_path("doctor-pending-migration-dimension-mismatch");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.execute("DROP TABLE memory_audit_log", []).unwrap();
    drop(connection);

    let mut command = base_binary_command(&db_path);
    command.env("LOCALHOLD_EMBEDDING_DIMENSIONS", "384");
    let output = command.args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_legacy_vectors_without_profile_even_when_migration_is_pending() {
    let db_path = unique_db_path("doctor-pending-profile");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    let memory = Memory::new_for_test("legacy vector".into(), vec![], Provenance::new_for_test(None, None, None), AccessPolicy::Public);
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(store.store(&memory, Some(&vec![0.1_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS])))
        .unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.execute("DROP TABLE embedding_profile", []).unwrap();
    drop(connection);

    let mut command = base_binary_command(&db_path);
    command.env("LOCALHOLD_EMBEDDING_BASE_URL", "https://configured.example/v1");
    command.env("LOCALHOLD_EMBEDDING_MODEL", "configured-model");
    command.env("LOCALHOLD_EMBEDDING_HEALTH_CHECK", "disabled");
    let output = command.args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_integrity_error_even_when_sqlite_migration_is_pending() {
    let db_path = unique_db_path("doctor-pending-integrity");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.pragma_update(None, "foreign_keys", false).unwrap();
    connection.execute("DROP TABLE memory_audit_log", []).unwrap();
    connection
        .execute("INSERT INTO memory_embedding_map (memory_id, vec_rowid) VALUES ('missing-memory', 999)", [])
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_malformed_core_schema_even_when_sqlite_migration_is_pending() {
    let db_path = unique_db_path("doctor-pending-malformed-core");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.execute("DROP TABLE memory_audit_log", []).unwrap();
    connection.execute("ALTER TABLE memories DROP COLUMN tags", []).unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_degrades_for_supported_legacy_sqlite_columns() {
    let db_path = unique_db_path("doctor-supported-legacy-column");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.execute("DROP INDEX idx_memories_memory_type", []).unwrap();
    connection.execute("ALTER TABLE memories DROP COLUMN memory_type", []).unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_for_mixed_legacy_and_current_impression_columns() {
    let db_path = unique_db_path("doctor-mixed-impression-columns");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch("ALTER TABLE memories ADD COLUMN access_count INTEGER; ALTER TABLE memories ADD COLUMN last_accessed_at TEXT;")
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_degrades_when_sqlite_embedding_map_fk_is_missing() {
    let db_path = unique_db_path("doctor-missing-map-fk");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE memory_embedding_map;
             CREATE TABLE memory_embedding_map (memory_id TEXT PRIMARY KEY, vec_rowid INTEGER NOT NULL UNIQUE);
             CREATE TRIGGER trg_memory_embedding_map_delete AFTER DELETE ON memory_embedding_map BEGIN
                 DELETE FROM memory_embeddings WHERE rowid = OLD.vec_rowid;
             END;",
        )
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_when_sqlite_embedding_map_fk_lacks_cascade() {
    let db_path = unique_db_path("doctor-map-fk-no-cascade");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE memory_embedding_map;
             CREATE TABLE memory_embedding_map (
                 memory_id TEXT PRIMARY KEY REFERENCES memories(id),
                 vec_rowid INTEGER NOT NULL UNIQUE
             );
             CREATE TRIGGER trg_memory_embedding_map_delete AFTER DELETE ON memory_embedding_map BEGIN
                 DELETE FROM memory_embeddings WHERE rowid = OLD.vec_rowid;
             END;",
        )
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_when_sqlite_embedding_map_has_extra_fk() {
    let db_path = unique_db_path("doctor-map-extra-fk");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "DROP TABLE memory_embedding_map;
             CREATE TABLE memory_embedding_map (
                 memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                 vec_rowid INTEGER NOT NULL UNIQUE REFERENCES memories(id)
             );
             CREATE TRIGGER trg_memory_embedding_map_delete AFTER DELETE ON memory_embedding_map BEGIN
                 DELETE FROM memory_embeddings WHERE rowid = OLD.vec_rowid;
             END;",
        )
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_when_sqlite_managed_table_name_is_a_view() {
    let db_path = unique_db_path("doctor-conflicting-view");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.execute("CREATE VIEW memories AS SELECT 'conflict' AS id", []).unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_for_same_name_broken_sqlite_trigger() {
    let db_path = unique_db_path("doctor-broken-trigger");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch("DROP TRIGGER trg_memory_fts_insert; CREATE TRIGGER trg_memory_fts_insert AFTER INSERT ON memories BEGIN SELECT 1; END;")
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_for_destructive_trigger_even_when_index_migration_is_pending() {
    let db_path = unique_db_path("doctor-pending-index-broken-trigger");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "DROP INDEX idx_memories_memory_type;
             DROP TRIGGER trg_memory_fts_insert;
             CREATE TRIGGER trg_memory_fts_insert AFTER INSERT ON memories BEGIN
                 INSERT INTO memory_fts(rowid, content) VALUES (NEW.rowid, NEW.content);
                 DELETE FROM memory_fts WHERE rowid <> NEW.rowid;
             END;",
        )
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_for_same_name_wrong_sqlite_index() {
    let db_path = unique_db_path("doctor-wrong-index");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch("DROP INDEX idx_memories_has_embedding; CREATE INDEX idx_memories_has_embedding ON memories(content);")
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_when_missing_sqlite_index_name_is_occupied_by_table() {
    let db_path = unique_db_path("doctor-index-name-conflict");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch("DROP INDEX idx_memories_has_embedding; CREATE TABLE idx_memories_has_embedding (dummy INTEGER);")
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_allows_repairable_missing_trigger_with_same_named_table() {
    let db_path = unique_db_path("doctor-trigger-separate-namespace");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch("DROP TRIGGER trg_memory_fts_insert; CREATE TABLE trg_memory_fts_insert (dummy INTEGER);")
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_for_malformed_current_sqlite_table_shape() {
    let db_path = unique_db_path("doctor-malformed-table");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.execute("ALTER TABLE memory_audit_log DROP COLUMN details", []).unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "failed");

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_degrades_when_sqlite_schema_needs_entity_migration() {
    let db_path = unique_db_path("doctor-entity-migration");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.execute("DROP TABLE memory_entities", []).unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "degraded");

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_degrades_when_sqlite_fts_objects_need_migration() {
    let db_path = unique_db_path("doctor-fts-migration");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "DROP TRIGGER trg_memory_fts_insert;
             DROP TRIGGER trg_memory_fts_update;
             DROP TRIGGER trg_memory_fts_delete;
             DROP TABLE memory_fts;",
        )
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "degraded");

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_when_sqlite_fts_shadow_conflicts_block_recreation() {
    let db_path = unique_db_path("doctor-fts-shadow-conflict");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "DROP TRIGGER trg_memory_fts_insert;
             DROP TRIGGER trg_memory_fts_update;
             DROP TRIGGER trg_memory_fts_delete;
             DROP TABLE memory_fts;
             CREATE TABLE memory_fts_data (id INTEGER);",
        )
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_for_internal_content_sqlite_fts_table() {
    let db_path = unique_db_path("doctor-internal-content-fts");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "DROP TRIGGER trg_memory_fts_insert;
             DROP TRIGGER trg_memory_fts_update;
             DROP TRIGGER trg_memory_fts_delete;
             DROP TABLE memory_fts;
             CREATE VIRTUAL TABLE memory_fts USING fts5(content);",
        )
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_ignores_sqlite_fts_options_hidden_in_comments() {
    let db_path = unique_db_path("doctor-comment-decoy-fts");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "DROP TRIGGER trg_memory_fts_insert;
             DROP TRIGGER trg_memory_fts_update;
             DROP TRIGGER trg_memory_fts_delete;
             DROP TABLE memory_fts;
             CREATE VIRTUAL TABLE memory_fts USING fts5(content /*, content=memories, content_rowid=rowid, */);",
        )
        .unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_when_nonempty_embedding_map_has_no_vector_table() {
    let db_path = unique_db_path("doctor-map-without-vectors");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    let memory = Memory::new_for_test("mapped vector".into(), vec![], Provenance::new_for_test(None, None, None), AccessPolicy::Public);
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(store.store(&memory, Some(&vec![0.1_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS])))
        .unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.execute("DROP TABLE memory_embeddings", []).unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_when_sqlite_vector_has_no_embedding_map_entry() {
    let db_path = unique_db_path("doctor-unmapped-vector");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    let embedding = vec![0.1_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS]
        .into_iter()
        .flat_map(f32::to_ne_bytes)
        .collect::<Vec<_>>();
    let _inserted = connection.execute("INSERT INTO memory_embeddings (embedding) VALUES (?1)", [embedding]).unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_for_noncanonical_sqlite_embedding_flag() {
    let db_path = unique_db_path("doctor-noncanonical-embedding-flag");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    let memory = Memory::new_for_test("bad embedding flag".into(), vec![], Provenance::new_for_test(None, None, None), AccessPolicy::Public);
    let id = tokio::runtime::Runtime::new().unwrap().block_on(store.store(&memory, None)).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    let _updated = connection.execute("UPDATE memories SET has_embedding = 2 WHERE id = ?1", [id.to_string()]).unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_degrades_when_sqlite_index_needs_migration() {
    let db_path = unique_db_path("doctor-index-migration");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.execute("DROP INDEX idx_memories_embedding_claim", []).unwrap();
    drop(connection);

    let output = base_binary_command(&db_path).args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "degraded");

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_for_incompatible_stored_embedding_profile_without_leaking_it() {
    let db_path = unique_db_path("doctor-profile-mismatch");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(store.verify_embedding_profile(&EmbeddingProfile::openai_compatible(
            "https://stored-secret.example/v1",
            "stored-secret-model",
            SqliteStore::DEFAULT_TEST_DIMENSIONS,
        )))
        .unwrap();
    drop(store);

    let mut command = base_binary_command(&db_path);
    command.env("LOCALHOLD_EMBEDDING_BASE_URL", "https://configured.example/v1");
    command.env("LOCALHOLD_EMBEDDING_MODEL", "configured-model");
    command.env("LOCALHOLD_EMBEDDING_HEALTH_CHECK", "disabled");
    let output = command.args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains("stored-secret"));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_fails_for_incompatible_stored_profile_when_embedding_map_is_missing() {
    let db_path = unique_db_path("doctor-profile-mismatch-missing-map");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(store.verify_embedding_profile(&EmbeddingProfile::openai_compatible(
            "https://stored.example/v1",
            "stored-model",
            SqliteStore::DEFAULT_TEST_DIMENSIONS,
        )))
        .unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection.execute("DROP TABLE memory_embedding_map", []).unwrap();
    drop(connection);

    let mut command = base_binary_command(&db_path);
    command.env("LOCALHOLD_EMBEDDING_BASE_URL", "https://configured.example/v1");
    command.env("LOCALHOLD_EMBEDDING_MODEL", "configured-model");
    command.env("LOCALHOLD_EMBEDDING_HEALTH_CHECK", "disabled");
    let output = command.args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_treats_embedding_rate_limit_as_reachable_like_startup() {
    let db_path = unique_db_path("doctor-embedding-rate-limit");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _peer) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let _bytes_read = stream.read(&mut request).unwrap();
        stream
            .write_all(b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });

    let mut command = base_binary_command(&db_path);
    command.env("LOCALHOLD_EMBEDDING_BASE_URL", format!("http://{address}/v1"));
    command.env("LOCALHOLD_EMBEDDING_MODEL", "rate-limited-model");
    let output = command.args(["doctor", "--json"]).output().unwrap();
    server.join().unwrap();
    assert!(output.status.success(), "rate-limited reachable provider should remain healthy");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "healthy");

    drop(store);
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_json_returns_failed_report_for_invalid_config_without_echoing_contents() {
    let db_path = unique_db_path("doctor-invalid-config");
    let mut cmd = base_binary_command(&db_path);
    cmd.env("LOCALHOLD_HTTP_AUTH_TOKEN", "must-not-appear\ninvalid-suffix");

    let output = cmd.args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["checks"][0]["name"], "build");
    assert_eq!(report["checks"][1]["name"], "configuration");
    assert!(!stdout.contains("must-not-appear"));
    assert!(!db_path.exists(), "failed configuration must not create storage");
}

#[test]
fn doctor_redacts_configured_embedding_identity_from_all_output() {
    let db_path = unique_db_path("doctor-redacted-identity");
    let mut command = base_binary_command(&db_path);
    command.env("LOCALHOLD_EMBEDDING_BASE_URL", "https://embeddings.example/v1/credential-ABC123");
    command.env("LOCALHOLD_EMBEDDING_MODEL", "model\n[failed] injected: credential-XYZ987");
    command.env("LOCALHOLD_EMBEDDING_HEALTH_CHECK", "disabled");
    let output = command.args(["doctor", "--json"]).output().unwrap();
    let combined = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(!combined.contains("credential-ABC123"));
    assert!(!combined.contains("credential-XYZ987"));
    assert!(!combined.contains("[failed] injected"));
}

#[test]
fn doctor_degrades_for_malformed_typed_env_without_echoing_value() {
    let db_path = unique_db_path("doctor-invalid-typed-env");
    let mut command = base_binary_command(&db_path);
    command.env("LOCALHOLD_DB_BACKEND", "postgresql-with-password-secret");
    let output = command.args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["name"] == "configuration" && check["status"] == "degraded")
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("postgresql-with-password-secret"));
}

#[test]
fn doctor_help_documents_side_effects_and_exit_codes() {
    let output = Command::new(env!("CARGO_BIN_EXE_hold")).args(["doctor", "--help"]).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("does not create databases"));
    assert!(stdout.contains("--allow-downloads"));
    assert!(stdout.contains("2  degraded"));
}

#[cfg(feature = "reranker")]
#[test]
fn doctor_does_not_create_or_download_reranker_cache_without_opt_in() {
    let db_path = unique_db_path("doctor-reranker-no-download");
    let cache_path = db_path.with_extension("model-cache");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let mut command = base_binary_command(&db_path);
    command.env("LOCALHOLD_RERANKER_ENABLED", "true");
    command.env("LOCALHOLD_RERANKER_CACHE_DIR", &cache_path);

    let output = command.args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(2_i32));
    assert!(!cache_path.exists(), "doctor must not create the reranker cache without --allow-downloads");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("--allow-downloads"));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[cfg(feature = "reranker")]
#[test]
fn doctor_fails_for_required_missing_direct_reranker_model() {
    let db_path = unique_db_path("doctor-reranker-required-direct");
    let missing_model = db_path.with_extension("missing.onnx");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);
    let mut command = base_binary_command(&db_path);
    command.env("LOCALHOLD_RERANKER_ENABLED", "true");
    command.env("LOCALHOLD_RERANKER_REQUIRED", "true");
    command.env("LOCALHOLD_RERANKER_MODEL_PATH", &missing_model);

    let output = command.args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));
    assert!(!missing_model.exists());

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn binary_starts_in_http_mode() {
    let db_path = unique_db_path("bin-http");
    let mut cmd = base_binary_command(&db_path);
    cmd.env("LOCALHOLD_TRANSPORT", "http");
    cmd.env("LOCALHOLD_HTTP_HOST", "127.0.0.1");
    cmd.env("LOCALHOLD_HTTP_PORT", "0");
    cmd.env("LOCALHOLD_HTTP_PATH", "/mcp");
    cmd.env("LOCALHOLD_LOG_LEVEL", "info");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().unwrap();
    let stderr = wait_for_startup_log(&mut child, "http", "localhold MCP server listening on");
    terminate_child(&mut child);
    let _stderr = stderr.join().unwrap();
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn binary_starts_text_only_without_embedding_failure_warning() {
    let db_path = unique_db_path("bin-text-only");
    let root = db_path.with_extension("config");
    let mut cmd = base_binary_command(&db_path);
    let _config_dir = isolate_user_config_dir(&mut cmd, &root);
    cmd.env("LOCALHOLD_TRANSPORT", "http");
    cmd.env("LOCALHOLD_HTTP_HOST", "127.0.0.1");
    cmd.env("LOCALHOLD_HTTP_PORT", "0");
    cmd.env("LOCALHOLD_LOG_LEVEL", "info");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().unwrap();
    let stderr_reader = wait_for_startup_log(&mut child, "text-only HTTP", "localhold MCP server listening on");
    terminate_child(&mut child);
    let stderr = stderr_reader.join().unwrap();

    assert!(stderr.contains("noop embedding provider initialized"), "expected text-only provider startup log: {stderr}");
    assert!(
        !stderr.contains("auto-reembed batch failed"),
        "text-only startup should not report embedding recovery failure: {stderr}"
    );

    let _cleanup = std::fs::remove_dir_all(root);
    let _cleanup = std::fs::remove_file(db_path);
}

#[cfg(not(feature = "reranker"))]
#[test]
fn required_reranker_fails_when_support_is_not_compiled() {
    let db_path = unique_db_path("bin-reranker-required-uncompiled");
    let root = db_path.with_extension("config");
    let mut cmd = base_binary_command(&db_path);
    let _config_dir = isolate_user_config_dir(&mut cmd, &root);
    cmd.env("LOCALHOLD_RERANKER_ENABLED", "true");
    cmd.env("LOCALHOLD_RERANKER_EXECUTION_PROVIDER", "cpu");
    cmd.env("LOCALHOLD_RERANKER_REQUIRED", "true");

    let output = cmd.output().unwrap();
    assert!(!output.status.success(), "required reranker should reject a binary without reranker support");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("compiled without the `reranker` feature"), "unexpected startup error: {stderr}");

    let _cleanup = std::fs::remove_dir_all(root);
    let _cleanup = std::fs::remove_file(db_path);
}

#[cfg(all(feature = "reranker", not(feature = "reranker-cuda")))]
#[test]
fn explicit_optional_cuda_stays_running_without_cpu_fallback() {
    let db_path = unique_db_path("bin-reranker-optional-cuda");
    let root = db_path.with_extension("config");
    let mut cmd = base_binary_command(&db_path);
    let _config_dir = isolate_user_config_dir(&mut cmd, &root);
    cmd.env("LOCALHOLD_RERANKER_ENABLED", "true");
    cmd.env("LOCALHOLD_RERANKER_EXECUTION_PROVIDER", "cuda");
    cmd.env("LOCALHOLD_RERANKER_REQUIRED", "false");
    cmd.env("LOCALHOLD_LOG_LEVEL", "info");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().unwrap();
    assert_child_stays_running(&mut child, "optional CUDA reranker in CPU binary");
    let _kill = child.kill();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("CUDA was requested but this binary was compiled without"),
        "unexpected fallback log: {stderr}"
    );
    assert!(
        !stderr.contains("reranker initialized (available: true)"),
        "explicit CUDA must not silently initialize a CPU reranker: {stderr}"
    );

    let _cleanup = std::fs::remove_dir_all(root);
    let _cleanup = std::fs::remove_file(db_path);
}

#[cfg(all(feature = "reranker", not(feature = "reranker-cuda")))]
#[test]
fn explicit_required_cuda_fails_without_cuda_support() {
    let db_path = unique_db_path("bin-reranker-required-cuda");
    let root = db_path.with_extension("config");
    let mut cmd = base_binary_command(&db_path);
    let _config_dir = isolate_user_config_dir(&mut cmd, &root);
    cmd.env("LOCALHOLD_RERANKER_ENABLED", "true");
    cmd.env("LOCALHOLD_RERANKER_EXECUTION_PROVIDER", "cuda");
    cmd.env("LOCALHOLD_RERANKER_REQUIRED", "true");

    let output = cmd.output().unwrap();
    assert!(!output.status.success(), "required CUDA reranker should reject a CPU-only binary");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("CUDA was requested but this binary was compiled without"),
        "unexpected startup error: {stderr}"
    );

    let _cleanup = std::fs::remove_dir_all(root);
    let _cleanup = std::fs::remove_file(db_path);
}

#[cfg(all(feature = "reranker", not(feature = "reranker-cuda")))]
#[test]
fn doctor_fails_required_cuda_before_cache_or_download_work() {
    let db_path = unique_db_path("doctor-reranker-required-cuda");
    let cache_path = db_path.with_extension("model-cache");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);

    for arguments in [["doctor", "--json"].as_slice(), ["doctor", "--json", "--allow-downloads"].as_slice()] {
        let mut command = base_binary_command(&db_path);
        command.env("LOCALHOLD_RERANKER_ENABLED", "true");
        command.env("LOCALHOLD_RERANKER_EXECUTION_PROVIDER", "cuda");
        command.env("LOCALHOLD_RERANKER_REQUIRED", "true");
        command.env("LOCALHOLD_RERANKER_CACHE_DIR", &cache_path);
        let output = command.args(arguments).output().unwrap();
        assert_eq!(output.status.code(), Some(1_i32));
        assert!(!cache_path.exists(), "impossible execution provider must fail before cache or download work");
        assert!(!String::from_utf8(output.stdout).unwrap().contains("rerun with --allow-downloads"));
    }

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
#[ignore = "requires Docker or local PostgreSQL with pgvector; set LOCALHOLD_POSTGRES_URL if not using the default smoke URL"]
fn binary_starts_with_postgres_backend() {
    let url = postgres_smoke_url();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hold"));
    cmd.env("LOCALHOLD_DB_BACKEND", "postgres");
    cmd.env("LOCALHOLD_POSTGRES_URL", url);
    cmd.env("LOCALHOLD_POSTGRES_MAX_CONNECTIONS", "1");
    cmd.env("LOCALHOLD_EMBEDDING_DIMENSIONS", "3");
    cmd.env("LOCALHOLD_TRANSPORT", "stdio");
    cmd.env("LOCALHOLD_LOG_LEVEL", "error");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().unwrap();
    let stderr = wait_for_startup_log(&mut child, "postgres stdio", "noop embedding provider initialized");
    terminate_child(&mut child);
    let _stderr = stderr.join().unwrap();
}

#[test]
fn binary_prints_migration_help_without_config() {
    for args in [&["migrate", "--help"][..], &["migrate", "sqlite-to-postgres", "--help"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_hold")).args(args).output().unwrap();

        assert!(output.status.success(), "help command failed: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stdout.contains("hold migrate sqlite-to-postgres"), "help output should include migration usage: {stdout}");
        assert!(stderr.is_empty(), "help command should not write stderr: {stderr}");
    }
}

#[test]
fn binary_prints_migration_usage_errors_once() {
    let output = Command::new(env!("CARGO_BIN_EXE_hold")).args(["migrate", "sqlite-to-postgres"]).output().unwrap();

    assert!(!output.status.success(), "usage error should fail");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("missing --sqlite"), "usage error should explain missing argument: {stderr}");
    assert!(!stderr.contains("Error:"), "usage error should not be printed by Result termination: {stderr}");
    assert!(!stderr.contains("Usage("), "usage error should not expose debug enum formatting: {stderr}");
}

#[test]
#[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
fn binary_migrates_sqlite_to_postgres() {
    let url = postgres_smoke_url();
    let source_path = unique_db_path("bin-migration-source");
    seed_sqlite_migration_source(&source_path);
    drop_postgres_smoke_schema(&url);

    let output = Command::new(env!("CARGO_BIN_EXE_hold"))
        .env("LOCALHOLD_POSTGRES_URL", &url)
        .args([
            "migrate",
            "sqlite-to-postgres",
            "--sqlite",
            source_path.to_str().unwrap(),
            "--embedding-dimensions",
            "3",
            "--yes",
        ])
        .output()
        .unwrap();

    assert!(output.status.success(), "migration command failed: {output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("SQLite to PostgreSQL migration complete"),
        "migration output should report completion: {stdout}"
    );
    assert_eq!(migrated_memory_count(&url), 1_i64, "expected one migrated memory row");
    let _cleanup = std::fs::remove_file(source_path);
}

#[test]
fn binary_exits_for_invalid_config_file() {
    let db_path = unique_db_path("bin-invalid-config");
    let root = db_path.with_extension("config");
    let mut cmd = base_binary_command(&db_path);
    let config_dir = isolate_user_config_dir(&mut cmd, &root).join("localhold");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_file = config_dir.join("localhold.toml");
    std::fs::write(&config_file, "[server]\ntransport = \"websocket\"\n").unwrap();

    cmd.current_dir(&root);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());

    let status = cmd.status().unwrap();
    assert!(!status.success());
    let _cleanup = std::fs::remove_dir_all(root);
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn binary_ignores_config_files_in_current_directory() {
    let db_path = unique_db_path("bin-ignore-cwd-config");
    let root = db_path.with_extension("cwd");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("localhold.toml"), "[server]\ntransport = \"websocket\"\n").unwrap();

    let mut cmd = base_binary_command(&db_path);
    let _config_dir = isolate_user_config_dir(&mut cmd, &root);
    cmd.current_dir(&root);
    cmd.env("LOCALHOLD_TRANSPORT", "stdio");
    cmd.env("LOCALHOLD_LOG_LEVEL", "info");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().unwrap();
    let stderr = wait_for_startup_log(&mut child, "stdio with untrusted CWD configs", "noop embedding provider initialized");
    terminate_child(&mut child);
    let _stderr = stderr.join().unwrap();
    let _cleanup = std::fs::remove_dir_all(root);
    let _cleanup = std::fs::remove_file(db_path);
}
