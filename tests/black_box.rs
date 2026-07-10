#![expect(missing_docs, reason = "black-box integration harness is an internal test crate")]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::header::{HeaderName, HeaderValue};
use rmcp::{
    ServiceExt as _,
    model::{CallToolRequestParams, CallToolResult, RawContent},
    service::RunningService,
    transport::{StreamableHttpClientTransport, TokioChildProcess, streamable_http_client::StreamableHttpClientTransportConfig},
};
use serde::de::DeserializeOwned;
use serde_json::json;
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt as _, BufReader},
    process::{Child, ChildStderr, Command},
    sync::Mutex,
};

const HTTP_AUTH_TOKEN: &str = "black-box-http-token";
const HTTP_PRINCIPAL: &str = "black-box-http-client";

#[tokio::test]
#[ignore = "black-box harness; run via just test-black-box"]
async fn stdio_black_box_noop_core_workflow() {
    let harness = BlackBoxHarness::spawn_stdio(BlackBoxConfig::noop("stdio-noop")).await;
    let client = harness.client();

    let remembered: RememberResponse = call_tool(
        client,
        "remember",
        json!({
            "content": "black box stdio lifecycle memory",
            "tags": ["black-box-stdio"],
            "scope": "black-box/stdio"
        }),
    )
    .await;

    let read: ReadResponse = call_tool(client, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.memory.content, "black box stdio lifecycle memory");

    let recall: RecallResponse = call_tool(client, "recall", json!({"query": "lifecycle", "tags": ["black-box-stdio"]})).await;
    assert_eq!(recall.search_mode, "keyword");
    assert_eq!(recall.count, 1);

    let count: CountResponse = call_tool(client, "admin_count", json!({"tags": ["black-box-stdio"]})).await;
    assert_eq!(count.total, 1);

    let deleted: DeleteResponse = call_tool(client, "forget", json!({"id": remembered.id})).await;
    assert!(deleted.deleted);

    let err = call_tool_error(client, "read", json!({"id": remembered.id})).await;
    assert!(err.contains("not found"));

    let logs = harness.shutdown().await;
    assert!(logs.contains("localhold starting up"), "expected startup log in stdio harness logs, got:\n{logs}");
}

#[tokio::test]
#[ignore = "black-box harness; run via just test-black-box"]
async fn http_black_box_noop_core_workflow() {
    let harness = BlackBoxHarness::spawn_http(BlackBoxConfig::noop("http-noop")).await;
    let client = harness.http_client().await;

    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 22, "expected all v2/admin tools exposed over HTTP");

    let batch: RememberManyResponse = call_tool(
        &client,
        "remember_many",
        json!({
            "memories": [
                {"content": "http batch alpha", "tags": ["black-box-http"], "scope": "black-box/http"},
                {"content": "http batch beta", "tags": ["black-box-http"], "scope": "black-box/http"}
            ]
        }),
    )
    .await;
    assert_eq!(batch.memories.len(), 2);

    let listed: AdminListResponse = call_tool(&client, "admin_list", json!({"tags": ["black-box-http"], "limit": 10_i32})).await;
    assert_eq!(listed.count, 2);

    let deleted: BulkDeleteResponse = call_tool(
        &client,
        "admin_bulk_delete",
        json!({
            "tags": ["black-box-http"],
            "include_superseded": true
        }),
    )
    .await;
    assert_eq!(deleted.deleted, 2);

    let count: CountResponse = call_tool(
        &client,
        "admin_count",
        json!({
            "tags": ["black-box-http"]
        }),
    )
    .await;
    assert_eq!(count.total, 0);

    let logs = harness.shutdown().await;
    assert!(logs.contains("listening on http://"), "expected HTTP startup log in harness logs, got:\n{logs}");
}

#[tokio::test]
#[ignore = "black-box harness; run via just test-black-box"]
async fn stdio_black_box_embedding_endpoint_unavailable_falls_back_cleanly() {
    let closed_port = 9_u16;
    let harness = BlackBoxHarness::spawn_stdio(BlackBoxConfig::embedding_endpoint_unavailable("stdio-embedding-unavailable", closed_port)).await;
    let client = harness.client();

    let remembered: RememberResponse = call_tool(
        client,
        "remember",
        json!({
            "content": "black box unavailable embedding fallback memory",
            "tags": ["black-box-embedding-unavailable"],
            "scope": "black-box/embedding-unavailable"
        }),
    )
    .await;

    let recall: RecallResponse = call_tool(
        client,
        "recall",
        json!({
            "query": "fallback memory",
            "tags": ["black-box-embedding-unavailable"],
            "limit": 3_i32
        }),
    )
    .await;

    assert_eq!(recall.search_mode, "keyword");
    assert_eq!(recall.count, 1);
    assert!(recall.results.iter().all(|result| result.diagnostics.reranker_score.is_none()));

    let cleanup: BulkDeleteResponse = call_tool(
        client,
        "admin_bulk_delete",
        json!({
            "tags": ["black-box-embedding-unavailable"],
            "include_superseded": true
        }),
    )
    .await;
    assert_eq!(cleanup.deleted, 1);

    let err = call_tool_error(client, "read", json!({"id": remembered.id})).await;
    assert!(err.contains("not found"));

    let logs = harness.shutdown().await;
    assert!(
        logs.contains("resilient embedding: inner provider is unavailable"),
        "expected degraded embedding log in harness logs, got:\n{logs}"
    );
}

#[tokio::test]
#[ignore = "black-box harness; run via just test-black-box"]
async fn stdio_black_box_reranker_emits_scores() {
    let harness = BlackBoxHarness::spawn_stdio(BlackBoxConfig::reranker("stdio-reranker")).await;
    let client = harness.client();

    let first: RememberResponse = call_tool(
        client,
        "remember",
        json!({
            "content": "black box reranker rust compiler ownership model",
            "tags": ["black-box-reranker"],
            "scope": "black-box/reranker"
        }),
    )
    .await;
    let second: RememberResponse = call_tool(
        client,
        "remember",
        json!({
            "content": "black box reranker rust compiler borrow checker details",
            "tags": ["black-box-reranker"],
            "scope": "black-box/reranker"
        }),
    )
    .await;

    await_has_embedding(client, &first.id, Duration::from_secs(30)).await;
    await_has_embedding(client, &second.id, Duration::from_secs(30)).await;

    let recall: RecallResponse = call_tool(
        client,
        "recall",
        json!({
            "query": "rust compiler ownership",
            "tags": ["black-box-reranker"],
            "limit": 2_i32
        }),
    )
    .await;

    assert_eq!(recall.search_mode, "hybrid");
    assert_eq!(recall.count, 2);
    assert!(
        recall.results.iter().all(|result| result.diagnostics.reranker_score.is_some()),
        "expected reranker_score on all results, got: {:?}",
        recall.results
    );

    let cleanup: BulkDeleteResponse = call_tool(
        client,
        "admin_bulk_delete",
        json!({
            "tags": ["black-box-reranker"],
            "include_superseded": true
        }),
    )
    .await;
    assert_eq!(cleanup.deleted, 2);

    let logs = harness.shutdown().await;
    assert!(
        logs.contains("reranker initialized (available: true)"),
        "expected reranker init log in harness logs, got:\n{logs}"
    );
}

#[tokio::test]
#[ignore = "black-box harness; run via just test-black-box"]
async fn stdio_black_box_reranker_misconfigured_degrades_cleanly() {
    let harness = BlackBoxHarness::spawn_stdio(BlackBoxConfig::reranker_misconfigured("stdio-reranker-broken")).await;
    let client = harness.client();

    let first: RememberResponse = call_tool(
        client,
        "remember",
        json!({
            "content": "black box broken reranker rust compiler ownership model",
            "tags": ["black-box-reranker-broken"],
            "scope": "black-box/reranker-broken"
        }),
    )
    .await;
    let second: RememberResponse = call_tool(
        client,
        "remember",
        json!({
            "content": "black box broken reranker rust compiler borrow checker details",
            "tags": ["black-box-reranker-broken"],
            "scope": "black-box/reranker-broken"
        }),
    )
    .await;

    await_has_embedding(client, &first.id, Duration::from_secs(30)).await;
    await_has_embedding(client, &second.id, Duration::from_secs(30)).await;

    let recall: RecallResponse = call_tool(
        client,
        "recall",
        json!({
            "query": "rust compiler ownership",
            "tags": ["black-box-reranker-broken"],
            "limit": 2_i32
        }),
    )
    .await;

    assert_eq!(recall.search_mode, "hybrid");
    assert_eq!(recall.count, 2);
    assert!(
        recall.results.iter().all(|result| result.diagnostics.reranker_score.is_none()),
        "expected no reranker scores when reranker config is broken, got: {:?}",
        recall.results
    );

    let cleanup: BulkDeleteResponse = call_tool(
        client,
        "admin_bulk_delete",
        json!({
            "tags": ["black-box-reranker-broken"],
            "include_superseded": true
        }),
    )
    .await;
    assert_eq!(cleanup.deleted, 2);

    let logs = harness.shutdown().await;
    assert!(
        logs.contains("reranker initialization failed after retries, continuing without"),
        "expected reranker fallback warning in harness logs, got:\n{logs}"
    );
}

#[tokio::test]
#[ignore = "black-box harness; run via just test-black-box"]
async fn http_black_box_reranker_emits_scores() {
    let harness = BlackBoxHarness::spawn_http(BlackBoxConfig::reranker("http-reranker")).await;
    let client = harness.http_client().await;

    let first: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "black box http reranker rust compiler ownership model",
            "tags": ["black-box-http-reranker"],
            "scope": "black-box/http-reranker"
        }),
    )
    .await;
    let second: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "black box http reranker rust compiler borrow checker details",
            "tags": ["black-box-http-reranker"],
            "scope": "black-box/http-reranker"
        }),
    )
    .await;

    await_has_embedding(&client, &first.id, Duration::from_secs(30)).await;
    await_has_embedding(&client, &second.id, Duration::from_secs(30)).await;

    let recall: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "rust compiler ownership",
            "tags": ["black-box-http-reranker"],
            "limit": 2_i32
        }),
    )
    .await;

    assert_eq!(recall.search_mode, "hybrid");
    assert_eq!(recall.count, 2);
    assert!(
        recall.results.iter().all(|result| result.diagnostics.reranker_score.is_some()),
        "expected reranker_score on all HTTP reranker results, got: {:?}",
        recall.results
    );

    let cleanup: BulkDeleteResponse = call_tool(
        &client,
        "admin_bulk_delete",
        json!({
            "tags": ["black-box-http-reranker"],
            "include_superseded": true
        }),
    )
    .await;
    assert_eq!(cleanup.deleted, 2);

    let logs = harness.shutdown().await;
    assert!(
        logs.contains("reranker initialized (available: true)"),
        "expected reranker init log in HTTP harness logs, got:\n{logs}"
    );
}

struct BlackBoxHarness {
    mode: HarnessMode,
    logs: Arc<Mutex<String>>,
    _tempdir: TempDir,
}

enum HarnessMode {
    Stdio { client: RunningService<rmcp::RoleClient, ()> },
    Http { child: Box<Child>, url: String },
}

impl BlackBoxHarness {
    async fn spawn_stdio(config: BlackBoxConfig) -> Self {
        let tempdir = tempfile::tempdir().unwrap();
        let db_path = unique_db_path(tempdir.path(), &config.label);
        let logs = Arc::new(Mutex::new(String::new()));

        let command = base_command(&db_path, tempdir.path(), &config);
        let (transport, stderr) = TokioChildProcess::builder(command).stderr(Stdio::piped()).spawn().unwrap();

        if let Some(stderr) = stderr {
            let _stderr_task = tokio::spawn(capture_stderr(stderr, Arc::clone(&logs)));
        }

        let client = ().serve(transport).await.unwrap();

        Self {
            mode: HarnessMode::Stdio { client },
            logs,
            _tempdir: tempdir,
        }
    }

    async fn spawn_http(config: BlackBoxConfig) -> Self {
        let tempdir = tempfile::tempdir().unwrap();
        let db_path = unique_db_path(tempdir.path(), &config.label);
        let logs = Arc::new(Mutex::new(String::new()));

        let mut command = base_command(&db_path, tempdir.path(), &config);
        let _configured = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .env("RECALL_TRANSPORT", "http")
            .env("RECALL_HTTP_HOST", "127.0.0.1")
            .env("RECALL_HTTP_PORT", "0")
            .env("RECALL_HTTP_PATH", "/mcp")
            .env("RECALL_HTTP_AUTH_TOKEN", HTTP_AUTH_TOKEN);

        let mut child = command.spawn().unwrap();
        if let Some(stderr) = child.stderr.take() {
            let _stderr_task = tokio::spawn(capture_stderr(stderr, Arc::clone(&logs)));
        }

        let url = wait_for_http_url(&logs, Duration::from_secs(30)).await;
        Self {
            mode: HarnessMode::Http { child: Box::new(child), url },
            logs,
            _tempdir: tempdir,
        }
    }

    #[expect(clippy::panic, reason = "test harness misuse should fail loudly")]
    fn client(&self) -> &RunningService<rmcp::RoleClient, ()> {
        match &self.mode {
            HarnessMode::Stdio { client } => client,
            HarnessMode::Http { .. } => panic!("HTTP harness does not expose a persistent stdio client"),
        }
    }

    #[expect(clippy::panic, reason = "test harness misuse should fail loudly")]
    async fn http_client(&self) -> RunningService<rmcp::RoleClient, ()> {
        let url = match &self.mode {
            HarnessMode::Http { url, .. } => url,
            HarnessMode::Stdio { .. } => panic!("stdio harness does not expose an HTTP URL"),
        };
        let mut headers = HashMap::new();
        let _previous = headers.insert(
            HeaderName::from_static(localhold::config::DEFAULT_HTTP_PRINCIPAL_HEADER),
            HeaderValue::from_static(HTTP_PRINCIPAL),
        );
        let config = StreamableHttpClientTransportConfig::with_uri(url.clone())
            .auth_header(HTTP_AUTH_TOKEN)
            .custom_headers(headers);
        let transport = StreamableHttpClientTransport::from_config(config);
        ().serve(transport).await.unwrap()
    }

    async fn shutdown(mut self) -> String {
        match &mut self.mode {
            HarnessMode::Stdio { client } => {
                let _closed = client.close().await;
            }
            HarnessMode::Http { child, .. } => {
                let _kill = child.kill().await;
                let _status = child.wait().await;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        self.logs.lock().await.clone()
    }
}

#[derive(Clone)]
struct BlackBoxConfig {
    label: String,
    env: Vec<(String, String)>,
    config_toml: String,
}

impl BlackBoxConfig {
    fn noop(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            env: Vec::new(),
            config_toml: format!(
                "[database]\npath = \"{db_path}\"\n\n[embedding]\nprovider = \"noop\"\ndimensions = 768\n\n[server]\ntransport = \"stdio\"\nlog_level = \"info\"\n",
                db_path = "__DB_PATH__"
            ),
        }
    }

    fn reranker(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            env: Vec::new(),
            config_toml: format!(
                "[database]\npath = \"{db_path}\"\n\n[embedding]\nprovider = \"openai_compatible\"\ndimensions = 768\n\n[embedding.openai_compatible]\nbase_url = \"http://localhost:11434/v1\"\nmodel = \"nomic-embed-text\"\n\n[server]\ntransport = \"stdio\"\nlog_level = \"info\"\n\n[search.reranker]\nenabled = true\n",
                db_path = "__DB_PATH__"
            ),
        }
    }

    fn reranker_misconfigured(label: &str) -> Self {
        Self {
            label: label.to_owned(),
            env: Vec::new(),
            config_toml: format!(
                "[database]\npath = \"{db_path}\"\n\n[embedding]\nprovider = \"openai_compatible\"\ndimensions = 768\n\n[embedding.openai_compatible]\nbase_url = \"http://localhost:11434/v1\"\nmodel = \"nomic-embed-text\"\n\n[server]\ntransport = \"stdio\"\nlog_level = \"info\"\n\n[search.reranker]\nenabled = true\nmodel_path = \"/definitely/missing/model.onnx\"\n",
                db_path = "__DB_PATH__"
            ),
        }
    }

    fn embedding_endpoint_unavailable(label: &str, port: u16) -> Self {
        Self {
            label: label.to_owned(),
            env: Vec::new(),
            config_toml: format!(
                "[database]\npath = \"{db_path}\"\n\n[embedding]\nprovider = \"openai_compatible\"\ndimensions = 768\n\n[embedding.openai_compatible]\nbase_url = \"http://127.0.0.1:{port}/v1\"\nmodel = \"nomic-embed-text\"\n\n[limits]\nembedding_timeout_secs = 1\n\n[server]\ntransport = \"stdio\"\nlog_level = \"info\"\n",
                db_path = "__DB_PATH__",
                port = port
            ),
        }
    }
}

fn base_command(db_path: &Path, cwd: &Path, config: &BlackBoxConfig) -> Command {
    let mut command = Command::new(binary_path());
    let config_dir = isolate_user_config_dir(&mut command, cwd);
    write_config(&config_dir, &config.config_toml.replace("__DB_PATH__", &escape_toml_string(db_path)));
    let _cwd = command.current_dir(cwd);
    for (key, value) in &config.env {
        let _env = command.env(key, value);
    }
    command
}

fn write_config(dir: &Path, contents: &str) {
    let localhold_dir = dir.join("localhold");
    std::fs::create_dir_all(&localhold_dir).unwrap();
    std::fs::write(localhold_dir.join("localhold.toml"), contents).unwrap();
}

fn isolate_user_config_dir(command: &mut Command, root: &Path) -> PathBuf {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let config_dir = root.join("user-config");
        let _env = command.env("XDG_CONFIG_HOME", &config_dir);
        config_dir
    }
    #[cfg(target_os = "macos")]
    {
        let _env = command.env("HOME", root);
        root.join("Library/Application Support")
    }
    #[cfg(windows)]
    {
        let config_dir = root.join("AppData/Roaming");
        let _env = command.env("APPDATA", &config_dir);
        config_dir
    }
}

fn escape_toml_string(path: &Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}

fn binary_path() -> PathBuf {
    std::env::var_os("LOCALHOLD_BLACK_BOX_BIN").map_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_hold")), PathBuf::from)
}

async fn capture_stderr(stderr: ChildStderr, logs: Arc<Mutex<String>>) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let mut guard = logs.lock().await;
        guard.push_str(&line);
        guard.push('\n');
    }
}

#[expect(clippy::arithmetic_side_effects, reason = "test helper deadline math with bounded durations")]
async fn wait_for_http_url(logs: &Arc<Mutex<String>>, timeout: Duration) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let snapshot = logs.lock().await.clone();
        if let Some(url) = extract_http_url(&snapshot) {
            return url;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for HTTP startup log, logs:\n{snapshot}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[expect(clippy::string_slice, reason = "the log prefix is ASCII and the byte index comes from matching that ASCII substring")]
#[expect(clippy::arithmetic_side_effects, reason = "ASCII prefix length arithmetic in test log parsing")]
fn extract_http_url(logs: &str) -> Option<String> {
    let needle = "listening on http://";
    for line in logs.lines() {
        if let Some(idx) = line.find(needle) {
            let url = &line[idx + "listening on ".len()..];
            return Some(url.trim().to_owned());
        }
    }
    None
}

#[expect(clippy::arithmetic_side_effects, reason = "test helper deadline math with bounded durations")]
async fn await_has_embedding(client: &RunningService<rmcp::RoleClient, ()>, id: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let read: ReadResponse = call_tool(client, "read", json!({"id": id})).await;
        if read.memory.has_embedding {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for embedding for memory {id}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn unique_db_path(root: &Path, name: &str) -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    root.join(format!("localhold-black-box-{name}-{}-{nanos}.db", std::process::id()))
}

#[expect(clippy::panic, reason = "test helper should fail loudly for non-object JSON arguments")]
fn call_tool_params(name: &str, args: serde_json::Value) -> CallToolRequestParams {
    let serde_json::Value::Object(args) = args else {
        panic!("tool args must be a JSON object");
    };
    CallToolRequestParams::new(name.to_owned()).with_arguments(args)
}

async fn call_tool<T: DeserializeOwned>(client: &RunningService<rmcp::RoleClient, ()>, name: &str, args: serde_json::Value) -> T {
    let result = client.call_tool(call_tool_params(name, args)).await.unwrap();
    assert!(!result.is_error.unwrap_or(false), "tool {name} returned error: {}", extract_text(&result));
    serde_json::from_str(extract_text(&result)).unwrap()
}

async fn call_tool_error(client: &RunningService<rmcp::RoleClient, ()>, name: &str, args: serde_json::Value) -> String {
    let result = client.call_tool(call_tool_params(name, args)).await.unwrap();
    assert!(result.is_error.unwrap_or(false), "expected error from {name}, got success");
    extract_text(&result).to_owned()
}

#[expect(clippy::panic, reason = "test helper should fail loudly if MCP response shape changes")]
fn extract_text(result: &CallToolResult) -> &str {
    assert!(!result.content.is_empty(), "MCP result has no content items");
    let RawContent::Text(text) = &result.content[0].raw else {
        panic!("expected text content");
    };
    &text.text
}

#[derive(Debug, serde::Deserialize)]
struct RememberResponse {
    id: String,
}

#[derive(Debug, serde::Deserialize)]
struct RememberManyResponse {
    memories: Vec<RememberManyItemResponse>,
}

#[derive(Debug, serde::Deserialize)]
struct RememberManyItemResponse {
    #[serde(rename = "id")]
    _id: String,
}

#[derive(Debug, serde::Deserialize)]
struct DeleteResponse {
    deleted: bool,
}

#[derive(Debug, serde::Deserialize)]
struct CountResponse {
    total: u64,
}

#[derive(Debug, serde::Deserialize)]
struct BulkDeleteResponse {
    deleted: u64,
}

#[derive(Debug, serde::Deserialize)]
struct AdminListResponse {
    count: usize,
}

#[derive(Debug, serde::Deserialize)]
struct ReadResponse {
    memory: MemoryResponse,
}

#[derive(Debug, serde::Deserialize)]
struct MemoryResponse {
    content: String,
    has_embedding: bool,
}

#[derive(Debug, serde::Deserialize)]
struct RecallResponse {
    search_mode: String,
    count: usize,
    results: Vec<RecallCard>,
}

#[derive(Debug, serde::Deserialize)]
struct RecallCard {
    diagnostics: MatchDiagnostics,
}

#[derive(Debug, serde::Deserialize)]
struct MatchDiagnostics {
    reranker_score: Option<f64>,
}
