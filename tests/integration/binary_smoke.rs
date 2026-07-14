use std::{
    io::{BufRead as _, BufReader, Read, Write as _},
    net::TcpListener,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use localhold::{
    config::PostgresDatabaseConfig,
    store::{EmbeddingProfile, MemoryWriter as _, PostgresStore, SqliteStore},
    types::{AccessPolicy, Memory, Provenance},
};
use rusqlite::params;
use serde_json::Value;
#[cfg(feature = "reranker")]
use sha2::{Digest as _, Sha256};
use sqlx_core::{query::query, query_scalar::query_scalar};
use sqlx_postgres::PgPoolOptions;
use zerocopy::IntoBytes as _;

const STDIO_INITIALIZE: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"localhold-binary-smoke","version":"1"}}}"#;

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

fn config_binary_command(root: &std::path::Path) -> (Command, std::path::PathBuf) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hold"));
    for (key, _value) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("LOCALHOLD_") {
            command.env_remove(key);
        }
    }
    let config_dir = isolate_user_config_dir(&mut command, root);
    command.env("LOCALHOLD_TEST_CONFIG_DIR", &config_dir);
    (command, config_dir)
}

#[cfg(feature = "reranker")]
fn models_binary_command(root: &std::path::Path, cache_dir: &std::path::Path) -> Command {
    let (mut command, _config_dir) = config_binary_command(root);
    command.env("LOCALHOLD_RERANKER_MODEL", "test/model");
    command.env("LOCALHOLD_RERANKER_REVISION", "revision");
    command.env("LOCALHOLD_RERANKER_CACHE_DIR", cache_dir);
    command.env("LOCALHOLD_RERANKER_MODEL_SHA256", sha256_hex(b"model artifact"));
    command.env("LOCALHOLD_RERANKER_TOKENIZER_SHA256", sha256_hex(b"tokenizer artifact"));
    command
}

#[cfg(feature = "reranker")]
fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len().saturating_mul(2));
    for byte in digest {
        output.push(char::from(HEX[usize::from(byte >> 4_u8)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f_u8)]));
    }
    output
}

#[test]
fn reranker_gate_help_is_offline_and_documents_thresholds() {
    let output = Command::new(env!("CARGO_BIN_EXE_hold")).args(["reranker", "gate", "--help"]).output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("CPU/CUDA parity"));
    assert!(stdout.contains("--max-vram-mib"));
    assert!(stdout.contains("one, four, and eight concurrent clients") || stdout.contains("concurrency"));
}

#[cfg(not(feature = "reranker-cuda"))]
#[test]
fn reranker_gate_requires_cuda_build_without_starting_server() {
    let root = unique_db_path("reranker-gate-no-cuda").with_extension("root");
    let (mut command, _config_dir) = config_binary_command(&root);
    let output = command.args(["reranker", "gate", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));
    assert!(String::from_utf8_lossy(&output.stderr).contains("built with reranker-cuda"));
    assert!(!root.exists(), "unsupported gate must not initialize configuration or storage");
}

#[cfg(feature = "reranker-cuda")]
#[test]
fn reranker_gate_invalid_threshold_is_structured_without_hardware_access() {
    let root = unique_db_path("reranker-gate-invalid-threshold").with_extension("root");
    let (mut command, _config_dir) = config_binary_command(&root);
    let output = command.args(["reranker", "gate", "--iterations", "0", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1_u32);
    assert_eq!(report["status"], "failed");
    assert_eq!(report["failures"][0], "iterations per client must be greater than zero");
    assert!(String::from_utf8(output.stderr).unwrap().is_empty());
    assert!(!root.exists(), "invalid gate options must not initialize storage or model artifacts");
}

#[cfg(feature = "reranker-cuda")]
#[test]
fn reranker_gate_rejects_malformed_environment_override() {
    let root = unique_db_path("reranker-gate-malformed-env").with_extension("root");
    let (mut command, _config_dir) = config_binary_command(&root);
    command.env("LOCALHOLD_RERANKER_PRECISION", "f16");
    let output = command.args(["reranker", "gate", "--iterations", "0", "--json"]).output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8(output.stderr).unwrap().contains("LOCALHOLD_* environment override is malformed"));
    assert!(!root.exists(), "malformed gate configuration must not initialize storage or model artifacts");
}

#[cfg(feature = "reranker")]
fn read_http_request_headers(stream: &mut impl Read) -> std::io::Result<Vec<u8>> {
    const MAX_HEADER_BYTES: usize = 16 * 1024;
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];
    while request.len() < MAX_HEADER_BYTES {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(request);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "model mock server received incomplete or oversized HTTP headers",
    ))
}

#[test]
fn config_paths_reports_canonical_policy_without_starting_server() {
    let root = unique_db_path("config-paths").with_extension("root");
    let (mut command, config_dir) = config_binary_command(&root);

    let output = command.args(["config", "paths", "--json"]).output().unwrap();

    assert!(output.status.success(), "config paths failed: {output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1_i32);
    assert_eq!(report["canonical_path"], config_dir.join("localhold/localhold.toml").to_string_lossy().as_ref());
    assert!(report["active_path"].is_null());
    assert_eq!(report["searched_paths"].as_array().unwrap().len(), 1_usize);
    assert!(String::from_utf8(output.stderr).unwrap().is_empty());
    assert!(!root.exists(), "config paths must not create the user configuration directory");
}

#[tokio::test]
async fn backup_restore_cli_round_trips_and_emits_stable_json() {
    let database = unique_db_path("backup-restore-cli");
    let backup_path = database.with_extension("backup.db");
    let store = SqliteStore::open(&database, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    let memory = Memory::new_for_test(
        "before restore".into(),
        Vec::new(),
        Provenance::new_for_test(Some("cli-test".into()), None, None),
        AccessPolicy::Public,
    );
    let memory_id = store.store(&memory, None).await.unwrap();

    let backup_output = base_binary_command(&database).arg("backup").arg(&backup_path).arg("--json").output().unwrap();
    assert!(backup_output.status.success(), "backup failed: {backup_output:?}");
    let backup_report: Value = serde_json::from_slice(&backup_output.stdout).unwrap();
    assert_eq!(backup_report["schema_version"], 1_i32);
    assert_eq!(backup_report["operation"], "backup");
    assert_eq!(backup_report["status"], "ok");
    assert_eq!(backup_report["database_schema_version"], 1_i32);
    assert_eq!(backup_report["memories"], 1_i32);
    assert!(backup_path.exists());
    drop(store);

    let connection = rusqlite::Connection::open(&database).unwrap();
    let _updated = connection
        .execute("UPDATE memories SET content = 'after backup' WHERE id = ?1", [memory_id.to_string()])
        .unwrap();
    drop(connection);

    let dry_run_output = base_binary_command(&database)
        .arg("restore")
        .arg(&backup_path)
        .args(["--dry-run", "--json"])
        .output()
        .unwrap();
    assert!(dry_run_output.status.success(), "restore dry-run failed: {dry_run_output:?}");
    let dry_run_report: Value = serde_json::from_slice(&dry_run_output.stdout).unwrap();
    assert_eq!(dry_run_report["status"], "validated");
    assert_eq!(dry_run_report["database_replaced"], false);
    let current: String = rusqlite::Connection::open(&database)
        .unwrap()
        .query_row("SELECT content FROM memories WHERE id = ?1", [memory_id.to_string()], |row| row.get(0))
        .unwrap();
    assert_eq!(current, "after backup");

    let restore_output = base_binary_command(&database).arg("restore").arg(&backup_path).args(["--yes", "--json"]).output().unwrap();
    assert!(restore_output.status.success(), "restore failed: {restore_output:?}");
    let restore_report: Value = serde_json::from_slice(&restore_output.stdout).unwrap();
    assert_eq!(restore_report["status"], "ok");
    assert_eq!(restore_report["database_replaced"], true);
    assert!(std::path::Path::new(restore_report["recovery_path"].as_str().unwrap()).exists());
    let restored: String = rusqlite::Connection::open(&database)
        .unwrap()
        .query_row("SELECT content FROM memories WHERE id = ?1", [memory_id.to_string()], |row| row.get(0))
        .unwrap();
    assert_eq!(restored, "before restore");
}

#[test]
fn restore_cli_reports_missing_confirmation_as_stable_json() {
    let database = unique_db_path("restore-confirmation");
    let backup_path = database.with_extension("backup.db");
    let output = base_binary_command(&database).arg("restore").arg(&backup_path).arg("--json").output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1_i32);
    assert_eq!(report["status"], "refused");
    assert!(report["message"].as_str().unwrap().contains("--yes"));
    assert!(output.stderr.is_empty());
    assert!(!database.exists());
}

#[test]
fn backup_cli_reports_unsupported_postgres_backend_as_stable_json() {
    let database = unique_db_path("backup-postgres-backend");
    let backup_path = database.with_extension("backup.db");
    let mut command = base_binary_command(&database);
    command.env("LOCALHOLD_DB_BACKEND", "postgres");
    command.env("LOCALHOLD_POSTGRES_URL", "postgres://localhold:secret@127.0.0.1/localhold");
    let output = command.arg("backup").arg(&backup_path).arg("--json").output().unwrap();
    assert_eq!(output.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1_i32);
    assert_eq!(report["operation"], "backup");
    assert_eq!(report["status"], "failed");
    assert!(report["message"].as_str().unwrap().contains("PostgreSQL-native"));
    assert!(output.stderr.is_empty());
    assert!(!database.exists());
    assert!(!backup_path.exists());
}

#[test]
fn config_init_creates_valid_config_and_refuses_to_clobber() {
    let root = unique_db_path("config-init").with_extension("root");
    let (mut init_command, config_dir) = config_binary_command(&root);
    let config_path = config_dir.join("localhold/localhold.toml");

    let output = init_command.args(["config", "init", "--json"]).output().unwrap();

    assert!(output.status.success(), "config init failed: {output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1_i32);
    assert_eq!(report["created"], true);
    assert_eq!(report["config_path"], config_path.to_string_lossy().as_ref());
    let original = std::fs::read_to_string(&config_path).unwrap();
    let _parsed: localhold::config::Config = toml::from_str(&original).unwrap();

    let (mut second_command, _config_dir) = config_binary_command(&root);
    let second = second_command.args(["config", "init"]).output().unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8(second.stdout).unwrap().contains("refusing to overwrite"));
    assert!(String::from_utf8(second.stderr).unwrap().is_empty());
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);

    let (mut json_command, _config_dir) = config_binary_command(&root);
    let json_failure = json_command.args(["config", "init", "--json"]).output().unwrap();
    assert_eq!(json_failure.status.code(), Some(1_i32));
    let failure_report: Value = serde_json::from_slice(&json_failure.stdout).unwrap();
    assert_eq!(failure_report["created"], false);
    assert_eq!(failure_report["exit_code"], 1_i32);
    assert!(failure_report["summary"].as_str().unwrap().contains("refusing to overwrite"));
    assert!(String::from_utf8(json_failure.stderr).unwrap().is_empty());
    let _cleanup = std::fs::remove_dir_all(root);
}

#[test]
fn config_validate_accepts_effective_starter_without_opening_database() {
    let root = unique_db_path("config-validate-valid").with_extension("root");
    let db_path = root.join("must-not-exist.db");
    let (mut init_command, _config_dir) = config_binary_command(&root);
    let init = init_command.args(["config", "init"]).output().unwrap();
    assert!(init.status.success());

    let (mut validate_command, _config_dir) = config_binary_command(&root);
    validate_command.env("LOCALHOLD_DB_PATH", &db_path);
    let output = validate_command.args(["config", "validate", "--json"]).output().unwrap();

    assert!(output.status.success(), "config validate failed: {output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["valid"], true);
    assert_eq!(report["exit_code"], 0_i32);
    assert!(!db_path.exists(), "config validate must not initialize storage");
    let _cleanup = std::fs::remove_dir_all(root);
}

#[test]
fn config_validate_rejects_invalid_file_without_leaking_parser_context() {
    let root = unique_db_path("config-validate-invalid").with_extension("root");
    let (mut command, config_dir) = config_binary_command(&root);
    let config_path = config_dir.join("localhold/localhold.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "[server]\nhttp_auth_token = \"parser-secret-ABC123\n").unwrap();

    let output = command.args(["config", "validate", "--json"]).output().unwrap();

    assert_eq!(output.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["valid"], false);
    assert_eq!(report["exit_code"], 1_i32);
    assert!(report["summary"].as_str().unwrap().contains("parser context was suppressed"));
    let combined = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(!combined.contains("parser-secret-ABC123"));
    let _cleanup = std::fs::remove_dir_all(root);
}

#[test]
fn config_validate_rejects_malformed_environment_override_without_echoing_value() {
    let root = unique_db_path("config-validate-env").with_extension("root");
    let (mut command, _config_dir) = config_binary_command(&root);
    command.env("LOCALHOLD_HTTP_PORT", "environment-secret-XYZ987");

    let output = command.args(["config", "validate", "--json"]).output().unwrap();

    assert_eq!(output.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["valid"], false);
    assert!(report["summary"].as_str().unwrap().contains("environment override is malformed"));
    let combined = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(!combined.contains("environment-secret-XYZ987"));
    let _cleanup = std::fs::remove_dir_all(root);
}

#[test]
fn config_help_and_unknown_commands_never_fall_through_to_server_startup() {
    let help = Command::new(env!("CARGO_BIN_EXE_hold")).args(["config", "--help"]).output().unwrap();
    assert!(help.status.success());
    let stdout = String::from_utf8(help.stdout).unwrap();
    assert!(stdout.contains("init [--json]"));
    assert!(stdout.contains("Exit codes:"));

    let root = unique_db_path("config-unknown").with_extension("root");
    let db_path = root.join("must-not-exist.db");
    let (mut command, _config_dir) = config_binary_command(&root);
    command.env("LOCALHOLD_DB_PATH", &db_path);
    let unknown = command.args(["config", "unknown"]).output().unwrap();
    assert!(!unknown.status.success());
    assert!(!db_path.exists(), "unknown config commands must not start or initialize the server");
}

#[test]
fn models_help_and_unknown_commands_never_fall_through_to_server_startup() {
    let help = Command::new(env!("CARGO_BIN_EXE_hold")).args(["models", "--help"]).output().unwrap();
    assert!(help.status.success());
    let stdout = String::from_utf8(help.stdout).unwrap();
    assert!(stdout.contains("verify [--json]"));
    assert!(stdout.contains("fetch --yes [--json]"));

    let root = unique_db_path("models-unknown").with_extension("root");
    let db_path = root.join("must-not-exist.db");
    let (mut command, _config_dir) = config_binary_command(&root);
    command.env("LOCALHOLD_DB_PATH", &db_path);
    let unknown = command.args(["models", "unknown"]).output().unwrap();
    assert!(!unknown.status.success());
    assert!(!db_path.exists(), "unknown models commands must not start or initialize the server");
}

#[cfg(feature = "reranker")]
#[test]
fn models_fetch_requires_confirmation_and_verify_is_offline() {
    let root = unique_db_path("models-refusal").with_extension("root");
    let cache = root.join("cache");

    let mut verify = models_binary_command(&root, &cache);
    let missing = verify.args(["models", "verify", "--json"]).output().unwrap();
    assert_eq!(missing.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(report["schema_version"], 1_i32);
    assert_eq!(report["status"], "missing");
    assert_eq!(report["network_allowed"], false);
    assert!(!cache.exists(), "offline verification must not create the model cache");

    let mut fetch = models_binary_command(&root, &cache);
    let refused = fetch.args(["models", "fetch", "--json"]).output().unwrap();
    assert_eq!(refused.status.code(), Some(1_i32));
    let refusal: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(refusal["status"], "refused");
    assert_eq!(refusal["network_allowed"], false);
    assert!(!cache.exists(), "refused fetch must not create the model cache");
}

#[cfg(feature = "reranker")]
#[test]
fn models_json_reports_invalid_configuration_and_malformed_overrides() {
    let invalid_root = unique_db_path("models-invalid-config").with_extension("root");
    let (mut invalid, _config_dir) = config_binary_command(&invalid_root);
    invalid.env("LOCALHOLD_RERANKER_ENABLED", "true");
    invalid.env("LOCALHOLD_RERANKER_MODEL", "custom/model");
    let invalid_output = invalid.args(["models", "verify", "--json"]).output().unwrap();
    assert_eq!(invalid_output.status.code(), Some(1_i32));
    let invalid_report: Value = serde_json::from_slice(&invalid_output.stdout).unwrap();
    assert_eq!(invalid_report["schema_version"], 1_i32);
    assert_eq!(invalid_report["command"], "verify");
    assert_eq!(invalid_report["status"], "error");
    assert_eq!(invalid_report["network_allowed"], false);
    assert_eq!(invalid_report["model"], "not_loaded");

    let malformed_root = unique_db_path("models-malformed-env").with_extension("root");
    let cache = malformed_root.join("cache");
    let model_dir = cache.join("test--model@revision");
    std::fs::create_dir_all(&model_dir).unwrap();
    std::fs::write(model_dir.join("model.onnx"), b"model artifact").unwrap();
    std::fs::write(model_dir.join("tokenizer.json"), b"tokenizer artifact").unwrap();
    let mut malformed = models_binary_command(&malformed_root, &cache);
    malformed.env("LOCALHOLD_RERANKER_PRECISION", "f16");
    let malformed_output = malformed.args(["models", "verify", "--json"]).output().unwrap();
    assert_eq!(malformed_output.status.code(), Some(1_i32));
    let malformed_report: Value = serde_json::from_slice(&malformed_output.stdout).unwrap();
    assert_eq!(malformed_report["command"], "verify");
    assert_eq!(malformed_report["status"], "error");
    assert_eq!(malformed_report["model"], "not_loaded");
    assert_ne!(malformed_report["status"], "verified");

    let _cleanup = std::fs::remove_dir_all(invalid_root);
    let _cleanup = std::fs::remove_dir_all(malformed_root);
}

#[cfg(feature = "reranker")]
#[test]
#[expect(clippy::panic, reason = "loopback fixture failures should include the unexpected request or socket error")]
fn models_fetch_downloads_only_when_explicit_and_reverifies_offline() {
    let root = unique_db_path("models-fetch").with_extension("root");
    let cache = root.join("cache");
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let started = std::time::Instant::now();
        let mut served = 0_u8;
        while served < 2 && started.elapsed() < Duration::from_secs(10) {
            let (mut stream, _peer) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("model mock server accept failed: {error}"),
            };
            let request = read_http_request_headers(&mut stream).unwrap();
            let request = String::from_utf8_lossy(&request);
            let body: &[u8] = if request.contains("/onnx/model.onnx") {
                b"model artifact"
            } else if request.contains("/tokenizer.json") {
                b"tokenizer artifact"
            } else {
                panic!("unexpected model download request: {request}");
            };
            let response = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
            served += 1;
        }
        served
    });

    let mut fetch = models_binary_command(&root, &cache);
    fetch.env("LOCALHOLD_TEST_RERANKER_BASE_URL", format!("http://{address}"));
    let output = fetch.args(["models", "fetch", "--yes", "--json"]).output().unwrap();
    let request_count = server.join().unwrap();
    assert!(output.status.success(), "models fetch failed: {output:?}");
    assert_eq!(request_count, 2, "fetch should request exactly the model and tokenizer artifacts");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "verified");
    assert_eq!(report["network_allowed"], true);
    assert_eq!(report["artifacts_changed"], true);

    let mut verify = models_binary_command(&root, &cache);
    let offline = verify.args(["models", "verify", "--json"]).output().unwrap();
    assert!(offline.status.success(), "offline verification failed: {offline:?}");
    let report: Value = serde_json::from_slice(&offline.stdout).unwrap();
    assert_eq!(report["status"], "verified");
    assert_eq!(report["network_allowed"], false);
    let _cleanup = std::fs::remove_dir_all(root);
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

#[expect(clippy::expect_used, reason = "test helper requires a piped stdin handle")]
fn initialize_stdio_child(child: &mut Child) {
    let stdin = child.stdin.as_mut().expect("stdio startup test must pipe stdin");
    writeln!(stdin, "{STDIO_INITIALIZE}").unwrap();
    stdin.flush().unwrap();
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
                embedding_profile,
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

fn seed_embedding_status_database(path: &std::path::Path, model: &str, embedded: usize, pending: usize) {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let store = SqliteStore::open(path, 3_usize).unwrap();
        let profile = EmbeddingProfile::openai_compatible("http://127.0.0.1:8000/v1", model, 3_usize);
        store.verify_embedding_profile(&profile).await.unwrap();
        for index in 0..embedded.saturating_add(pending) {
            let memory = Memory::new_for_test(format!("embedding status memory {index}"), Vec::new(), Provenance::default(), AccessPolicy::Public);
            let embedding = (index < embedded).then_some([0.1_f32, 0.2_f32, 0.3_f32]);
            let _id = store.store(&memory, embedding.as_ref().map(<[_; 3]>::as_slice)).await.unwrap();
        }
    });
}

fn create_canceling_embedding_corruption(path: &std::path::Path) {
    let connection = rusqlite::Connection::open(path).unwrap();
    let mapped_rowid: i64 = connection.query_row("SELECT vec_rowid FROM memory_embedding_map LIMIT 1", [], |row| row.get(0)).unwrap();
    let _deleted = connection.execute("DELETE FROM memory_embeddings WHERE rowid = ?1", params![mapped_rowid]).unwrap();
    let orphan = [0.7_f32, 0.8_f32, 0.9_f32];
    let _inserted = connection
        .execute("INSERT INTO memory_embeddings (embedding) VALUES (?1)", params![orphan.as_bytes()])
        .unwrap();
}

fn replace_vector_table_with_malformed_table(path: &std::path::Path) {
    let connection = rusqlite::Connection::open(path).unwrap();
    connection
        .execute_batch("DROP TABLE memory_embeddings; CREATE TABLE memory_embeddings (embedding BLOB NOT NULL);")
        .unwrap();
}

fn seed_postgres_embedding_status_database(url: &str, model: &str) {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut config = PostgresDatabaseConfig::default();
        config.url = url.into();
        config.max_connections = 1;
        let store = PostgresStore::open(&config, 3_usize).await.unwrap();
        let profile = EmbeddingProfile::openai_compatible("http://127.0.0.1:8000/v1", model, 3_usize);
        store.verify_embedding_profile(&profile).await.unwrap();
        for index in 0..2_usize {
            let memory = Memory::new_for_test(format!("postgres embedding status memory {index}"), Vec::new(), Provenance::default(), AccessPolicy::Public);
            let embedding = (index == 0).then_some([0.1_f32, 0.2_f32, 0.3_f32]);
            let _id = store.store(&memory, embedding.as_ref().map(<[_; 3]>::as_slice)).await.unwrap();
        }
    });
}

fn drop_postgres_embedding_tables(url: &str) {
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let pool = PgPoolOptions::new().max_connections(1).connect(url).await.unwrap();
        let _dropped = query("DROP TABLE embedding_profile, memory_embeddings").execute(&pool).await.unwrap();
    });
}

fn embedding_status_command(db_path: &std::path::Path, model: &str) -> Command {
    let mut command = base_binary_command(db_path);
    command.env("LOCALHOLD_EMBEDDING_BASE_URL", "http://127.0.0.1:8000/v1");
    command.env("LOCALHOLD_EMBEDDING_MODEL", model);
    command.env("LOCALHOLD_EMBEDDING_DIMENSIONS", "3");
    command.env("LOCALHOLD_EMBEDDING_HEALTH_CHECK", "disabled");
    command.env("LOCALHOLD_EMBEDDING_API_KEY", "embedding-status-secret");
    command
}

fn noop_embedding_status_command(db_path: &std::path::Path, dimensions: &str) -> Command {
    let mut command = base_binary_command(db_path);
    for key in [
        "LOCALHOLD_EMBEDDING_BASE_URL",
        "LOCALHOLD_EMBEDDING_MODEL",
        "LOCALHOLD_EMBEDDING_API_KEY",
        "LOCALHOLD_EMBEDDING_AUTH_MODE",
        "LOCALHOLD_EMBEDDING_SEND_DIMENSIONS",
        "LOCALHOLD_EMBEDDING_HEALTH_CHECK",
        "LOCALHOLD_EMBEDDING_ALLOW_INSECURE_HTTP",
    ] {
        command.env_remove(key);
    }
    command.env("LOCALHOLD_EMBEDDING_DIMENSIONS", dimensions);
    command
}

fn postgres_embedding_status_command(url: &str, root: &std::path::Path, auto_migrate: bool) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hold"));
    let _config_dir = isolate_user_config_dir(&mut command, root);
    command.env("LOCALHOLD_DB_BACKEND", "postgres");
    command.env("LOCALHOLD_POSTGRES_URL", url);
    command.env("LOCALHOLD_POSTGRES_MAX_CONNECTIONS", "1");
    command.env("LOCALHOLD_POSTGRES_AUTO_MIGRATE", auto_migrate.to_string());
    command.env("LOCALHOLD_EMBEDDING_BASE_URL", "http://127.0.0.1:8000/v1");
    command.env("LOCALHOLD_EMBEDDING_MODEL", "embed-postgres");
    command.env("LOCALHOLD_EMBEDDING_DIMENSIONS", "3");
    command.env("LOCALHOLD_EMBEDDING_HEALTH_CHECK", "disabled");
    command
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
    initialize_stdio_child(&mut child);
    let stderr = wait_for_startup_log(&mut child, "stdio", "localhold MCP server running on stdio");
    terminate_child(&mut child);
    let _stderr = stderr.join().unwrap();
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn embeddings_status_json_reports_rebuild_progress_and_secret_free_profiles() {
    let db_path = unique_db_path("embeddings-status-rebuilding");
    seed_embedding_status_database(&db_path, "embed-current", 1, 2);

    let output = embedding_status_command(&db_path, "embed-current")
        .args(["embeddings", "status", "--json"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2_i32), "pending vectors should report degraded rebuild status: {output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["schema_version"], 1_i32);
    assert_eq!(report["status"], "degraded");
    assert_eq!(report["state"], "rebuilding");
    assert_eq!(report["provider_health"], "check_disabled");
    assert_eq!(report["configured_profile"]["model"], "embed-current");
    assert_eq!(report["stored_profile"]["model"], "embed-current");
    assert_eq!(report["counts"]["total_memories"], 3_i32);
    assert_eq!(report["counts"]["embedded_memories"], 1_i32);
    assert_eq!(report["counts"]["pending_memories"], 2_i32);
    assert_eq!(report["counts"]["vector_rows"], 1_i32);
    assert!(!stdout.contains("embedding-status-secret"), "status JSON must not contain API keys");

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn embeddings_status_requires_reindex_for_profile_mismatch() {
    let db_path = unique_db_path("embeddings-status-mismatch");
    seed_embedding_status_database(&db_path, "embed-old", 1, 0);

    let output = embedding_status_command(&db_path, "embed-new").args(["embeddings", "status", "--json"]).output().unwrap();

    assert_eq!(output.status.code(), Some(1_i32), "profile mismatch must fail: {output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "failed");
    assert_eq!(report["state"], "reindex_required");
    assert_eq!(report["configured_profile"]["model"], "embed-new");
    assert_eq!(report["stored_profile"]["model"], "embed-old");
    assert!(report["summary"].as_str().unwrap().contains("hold embeddings reindex --yes"));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn embeddings_status_detects_missing_and_orphan_vectors_even_when_counts_cancel() {
    let db_path = unique_db_path("embeddings-status-canceling-corruption");
    seed_embedding_status_database(&db_path, "embed-current", 1, 0);
    create_canceling_embedding_corruption(&db_path);

    let output = embedding_status_command(&db_path, "embed-current")
        .args(["embeddings", "status", "--json"])
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(1_i32),
        "relational corruption must fail even when aggregate row counts match: {output:?}"
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["state"], "inconsistent");
    assert_eq!(report["counts"]["embedded_memories"], 1_i32);
    assert_eq!(report["counts"]["mapped_memories"], 1_i32);
    assert_eq!(report["counts"]["vector_rows"], 1_i32);
    assert_eq!(report["counts"]["missing_vectors"], 1_i32);
    assert_eq!(report["counts"]["unexpected_vectors"], 1_i32);

    let text_output = embedding_status_command(&db_path, "embed-current").args(["embeddings", "status"]).output().unwrap();
    assert_eq!(text_output.status.code(), Some(1_i32));
    assert!(String::from_utf8(text_output.stdout).unwrap().contains("Inconsistencies: missing 1, unexpected 1"));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn embeddings_status_rejects_an_unparseable_empty_vector_table() {
    let db_path = unique_db_path("embeddings-status-malformed-vector-table");
    seed_embedding_status_database(&db_path, "embed-current", 0, 1);
    replace_vector_table_with_malformed_table(&db_path);

    let output = embedding_status_command(&db_path, "embed-current")
        .args(["embeddings", "status", "--json"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["state"], "reindex_required");
    assert_eq!(report["stored_dimensions"], Value::Null);
    assert!(report["summary"].as_str().unwrap().contains("dimensions could not be read"));

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn embeddings_status_checks_vector_dimensions_before_declaring_noop_disabled() {
    let db_path = unique_db_path("embeddings-status-noop-dimension-mismatch");
    seed_embedding_status_database(&db_path, "embed-current", 0, 0);

    let output = noop_embedding_status_command(&db_path, "4").args(["embeddings", "status", "--json"]).output().unwrap();

    assert_eq!(output.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["provider_health"], "disabled");
    assert_eq!(report["state"], "reindex_required");
    assert_eq!(report["stored_dimensions"], 3_i32);

    let _cleanup = std::fs::remove_file(db_path.with_extension("db-shm"));
    let _cleanup = std::fs::remove_file(db_path.with_extension("db-wal"));
    let _cleanup = std::fs::remove_file(db_path);
}

#[test]
fn embeddings_status_does_not_initialize_a_missing_database() {
    let db_path = unique_db_path("embeddings-status-missing");

    let output = embedding_status_command(&db_path, "embed-current")
        .args(["embeddings", "status", "--json"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["state"], "not_initialized");
    assert!(!db_path.exists(), "status must not create a missing SQLite database");
}

#[test]
fn embeddings_status_help_documents_json_and_exit_codes_without_loading_config() {
    let output = Command::new(env!("CARGO_BIN_EXE_hold")).args(["embeddings", "status", "--help"]).output().unwrap();

    assert!(output.status.success(), "help command failed: {output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("status [--json]"));
    assert!(stdout.contains("Exit codes for status"));
    assert!(String::from_utf8(output.stderr).unwrap().is_empty());
}

#[test]
#[ignore = "requires Docker or local PostgreSQL with pgvector; destructive cleanup requires LOCALHOLD_ALLOW_DESTRUCTIVE_PG_SMOKE=1"]
fn embeddings_status_reports_postgres_profile_and_progress() {
    let url = postgres_smoke_url();
    drop_postgres_smoke_schema(&url);
    seed_postgres_embedding_status_database(&url, "embed-postgres");
    let root = unique_db_path("embeddings-status-postgres").with_extension("config");
    let output = postgres_embedding_status_command(&url, &root, true)
        .args(["embeddings", "status", "--json"])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2_i32), "one pending PostgreSQL memory should report rebuilding: {output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["backend"], "postgres");
    assert_eq!(report["state"], "rebuilding");
    assert_eq!(report["stored_profile"]["model"], "embed-postgres");
    assert_eq!(report["counts"]["embedded_memories"], 1_i32);
    assert_eq!(report["counts"]["pending_memories"], 1_i32);
    assert_eq!(report["counts"]["vector_rows"], 1_i32);

    drop_postgres_embedding_tables(&url);
    let no_migration_output = postgres_embedding_status_command(&url, &root, false)
        .args(["embeddings", "status", "--json"])
        .output()
        .unwrap();
    assert_eq!(no_migration_output.status.code(), Some(1_i32));
    let no_migration_report: Value = serde_json::from_slice(&no_migration_output.stdout).unwrap();
    assert_eq!(no_migration_report["state"], "unavailable");

    drop_postgres_smoke_schema(&url);
    let _cleanup = std::fs::remove_dir_all(root);
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

#[cfg(feature = "reranker-cuda")]
#[test]
fn doctor_reports_missing_cuda_runtime_without_panicking() {
    let root = unique_db_path("doctor-missing-ort").with_extension("root");
    let model_path = root.join("model.onnx");
    let tokenizer_path = root.join("tokenizer.json");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&model_path, b"model fixture").unwrap();
    std::fs::write(tokenizer_path, b"tokenizer fixture").unwrap();
    let (mut command, _config_dir) = config_binary_command(&root);
    command.env("LOCALHOLD_RERANKER_ENABLED", "true");
    command.env("LOCALHOLD_RERANKER_REQUIRED", "true");
    command.env("LOCALHOLD_RERANKER_EXECUTION_PROVIDER", "cuda");
    command.env("LOCALHOLD_RERANKER_MODEL_PATH", &model_path);
    command.env("ORT_DYLIB_PATH", root.join("missing-libonnxruntime.so"));

    let output = command.args(["doctor", "--json"]).output().unwrap();

    assert_eq!(output.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let reranker = report["checks"].as_array().unwrap().iter().find(|check| check["name"] == "reranker").unwrap();
    assert_eq!(reranker["status"], "failed");
    assert!(reranker["summary"].as_str().unwrap().contains("ORT_DYLIB_PATH"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));

    let _cleanup = std::fs::remove_dir_all(root);
}

#[cfg(feature = "reranker-cuda")]
#[test]
fn doctor_reports_incompatible_cuda_runtime_without_panicking() {
    let root = unique_db_path("doctor-incompatible-ort").with_extension("root");
    let model_path = root.join("model.onnx");
    let tokenizer_path = root.join("tokenizer.json");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(&model_path, b"model fixture").unwrap();
    std::fs::write(tokenizer_path, b"tokenizer fixture").unwrap();
    let (mut command, _config_dir) = config_binary_command(&root);
    command.env("LOCALHOLD_RERANKER_ENABLED", "true");
    command.env("LOCALHOLD_RERANKER_REQUIRED", "true");
    command.env("LOCALHOLD_RERANKER_EXECUTION_PROVIDER", "cuda");
    command.env("LOCALHOLD_RERANKER_MODEL_PATH", &model_path);
    command.env("ORT_DYLIB_PATH", loadable_non_ort_library());

    let output = command.args(["doctor", "--json"]).output().unwrap();

    assert_eq!(output.status.code(), Some(1_i32));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let reranker = report["checks"].as_array().unwrap().iter().find(|check| check["name"] == "reranker").unwrap();
    assert_eq!(reranker["status"], "failed");
    assert!(reranker["summary"].as_str().unwrap().contains("compatible ONNX Runtime"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("panicked"));

    let _cleanup = std::fs::remove_dir_all(root);
}

#[cfg(all(feature = "reranker-cuda", target_os = "windows"))]
const fn loadable_non_ort_library() -> &'static str {
    "kernel32.dll"
}

#[cfg(all(feature = "reranker-cuda", any(target_os = "linux", target_os = "android")))]
const fn loadable_non_ort_library() -> &'static str {
    "libc.so.6"
}

#[cfg(all(feature = "reranker-cuda", any(target_os = "macos", target_os = "ios")))]
const fn loadable_non_ort_library() -> &'static str {
    "libSystem.B.dylib"
}

#[cfg(all(
    feature = "reranker-cuda",
    not(any(target_os = "windows", target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios"))
))]
const fn loadable_non_ort_library() -> &'static str {
    "libc.so"
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
    initialize_stdio_child(&mut child);
    let stderr_reader = wait_for_startup_log(&mut child, "optional CUDA reranker in CPU binary", "localhold MCP server running on stdio");
    terminate_child(&mut child);
    let stderr = stderr_reader.join().unwrap();
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
    let root = unique_db_path("bin-postgres").with_extension("config");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hold"));
    let _config_dir = isolate_user_config_dir(&mut cmd, &root);
    cmd.env("LOCALHOLD_DB_BACKEND", "postgres");
    cmd.env("LOCALHOLD_POSTGRES_URL", url);
    cmd.env("LOCALHOLD_POSTGRES_MAX_CONNECTIONS", "1");
    cmd.env("LOCALHOLD_EMBEDDING_DIMENSIONS", "3");
    cmd.env("LOCALHOLD_TRANSPORT", "stdio");
    cmd.env("LOCALHOLD_LOG_LEVEL", "info");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn().unwrap();
    initialize_stdio_child(&mut child);
    let stderr = wait_for_startup_log(&mut child, "postgres stdio", "localhold MCP server running on stdio");
    terminate_child(&mut child);
    let _stderr = stderr.join().unwrap();
    let _cleanup = std::fs::remove_dir_all(root);
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
    initialize_stdio_child(&mut child);
    let stderr = wait_for_startup_log(&mut child, "stdio with untrusted CWD configs", "localhold MCP server running on stdio");
    terminate_child(&mut child);
    let _stderr = stderr.join().unwrap();
    let _cleanup = std::fs::remove_dir_all(root);
    let _cleanup = std::fs::remove_file(db_path);
}
