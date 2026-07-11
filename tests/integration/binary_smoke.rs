use std::{
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use localhold::{
    store::{MemoryWriter as _, SqliteStore},
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
    cmd.env("LOCALHOLD_DB_PATH", db_path);
    cmd.env("LOCALHOLD_LOG_LEVEL", "error");
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

fn assert_child_stays_running(child: &mut Child, label: &str) {
    // Startup should succeed and keep serving until explicitly terminated.
    thread::sleep(Duration::from_millis(300));
    let status = child.try_wait().unwrap();
    assert!(status.is_none(), "{label} process exited early: {status:?}");
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
                memory_v2_metadata,
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
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());

    let mut child = cmd.spawn().unwrap();
    assert_child_stays_running(&mut child, "stdio");
    terminate_child(&mut child);
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
fn doctor_reports_healthy_for_current_sqlite_database() {
    let db_path = unique_db_path("doctor-healthy");
    let store = SqliteStore::open(&db_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    drop(store);

    let output = base_binary_command(&db_path).arg("doctor").output().unwrap();
    assert!(
        output.status.success(),
        "current SQLite database should be healthy: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("LocalHold doctor: healthy"));
    assert!(stdout.contains("[healthy] storage:"));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn doctor_json_returns_failed_report_for_invalid_config_without_echoing_contents() {
    let db_path = unique_db_path("doctor-invalid-config");
    let root = db_path.with_extension("config");
    let mut cmd = base_binary_command(&db_path);
    let config_dir = isolate_user_config_dir(&mut cmd, &root).join("localhold");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("localhold.toml"), "[server]\ntransport = \"websocket\"\n# must-not-appear\n").unwrap();

    let output = cmd.args(["doctor", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert!(!stdout.contains("must-not-appear"));
    assert!(!db_path.exists(), "failed configuration must not create storage");

    let _cleanup = std::fs::remove_dir_all(root);
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

#[test]
fn binary_starts_in_http_mode() {
    let db_path = unique_db_path("bin-http");
    let mut cmd = base_binary_command(&db_path);
    cmd.env("LOCALHOLD_TRANSPORT", "http");
    cmd.env("LOCALHOLD_HTTP_HOST", "127.0.0.1");
    cmd.env("LOCALHOLD_HTTP_PORT", "0");
    cmd.env("LOCALHOLD_HTTP_PATH", "/mcp");
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());

    let mut child = cmd.spawn().unwrap();
    assert_child_stays_running(&mut child, "http");
    terminate_child(&mut child);
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
    assert_child_stays_running(&mut child, "text-only HTTP");
    let _kill = child.kill();
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();

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
    cmd.stderr(Stdio::null());

    let mut child = cmd.spawn().unwrap();
    assert_child_stays_running(&mut child, "postgres stdio");
    terminate_child(&mut child);
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
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());

    let mut child = cmd.spawn().unwrap();
    assert_child_stays_running(&mut child, "stdio with untrusted CWD configs");
    terminate_child(&mut child);
    let _cleanup = std::fs::remove_dir_all(root);
    let _cleanup = std::fs::remove_file(db_path);
}
