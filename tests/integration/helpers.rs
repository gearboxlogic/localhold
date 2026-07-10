use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::http::{HeaderName, HeaderValue};
use localhold::{
    clock::Clock,
    config::{AnonymousPolicy, DEFAULT_HTTP_PRINCIPAL_HEADER, LimitsConfig, SearchConfig, ServerConfig},
    embedding::{BoxFuture, EmbeddingProvider, NoopEmbedding},
    engine::RecallEngine,
    error::EmbeddingError,
    http_transport::build_router,
    server::{HttpPrincipalSource, RecallServer},
    store::{MemoryWriter as _, SqliteStore},
    types::{AccessPolicy, Memory, MemoryId, Provenance},
};
use parking_lot::Mutex;
use rmcp::{
    ServiceExt as _,
    model::{CallToolRequestParams, CallToolResult},
    service::RunningService,
    transport::{StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig},
};
use serde::de::DeserializeOwned;
use tokio_util::sync::CancellationToken;

pub(crate) const TEST_HTTP_AUTH_TOKEN: &str = "localhold-integration-token";
pub(crate) const TEST_HTTP_PRINCIPAL: &str = "localhold-integration-client";

/// Poll until all background embedding tasks complete or timeout is reached.
/// Panics if tasks don't complete within the timeout.
#[expect(clippy::arithmetic_side_effects, reason = "test helper: Instant + Duration cannot overflow in practice")]
pub(crate) async fn await_embeddings(server: &RecallServer, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = server.tracked_task_count();
        if remaining == 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "await_embeddings timed out with {remaining} tasks still pending after {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
        let _reaped = server.reap_completed_tasks_for_test();
    }
}

/// Embedding provider that returns deterministic 768-dim vectors.
/// Uses FNV-1a hash of the input text to seed the vector, so identical text
/// produces identical embeddings and different text produces different embeddings.
pub(crate) struct DeterministicEmbedding;

/// Generate a deterministic 768-dim embedding from text using FNV-1a hashing.
#[expect(clippy::float_arithmetic, reason = "intentional float math for deterministic test embedding generation")]
fn deterministic_embed(text: &str) -> Vec<f32> {
    let mut embedding = vec![0.0_f32; 768];
    let hash = fnv1a(text);
    for (i, val) in embedding.iter_mut().enumerate() {
        #[expect(clippy::as_conversions, reason = "usize index always fits in u64")]
        let seed = hash.wrapping_add(i as u64);
        #[expect(clippy::as_conversions, reason = "intentional u64→f32 cast for deterministic embedding seed")]
        #[expect(clippy::cast_precision_loss, reason = "intentional u64→f32 cast for deterministic embedding seed")]
        #[expect(clippy::integer_division_remainder_used, reason = "intentional modular arithmetic for hash-based embedding seed")]
        {
            *val = ((seed % 20_000) as f32 / 10_000.0) - 1.0;
        }
    }
    // Normalize to unit length
    let norm = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for val in &mut embedding {
            *val /= norm;
        }
    }
    embedding
}

impl EmbeddingProvider for DeterministicEmbedding {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        Box::pin(async move { Ok(deterministic_embed(text)) })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
        Box::pin(async { Ok(()) })
    }
}

fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

#[derive(Clone, Debug)]
pub(crate) struct ScriptedRule {
    embedding: Vec<f32>,
    delay: Duration,
}

impl ScriptedRule {
    #[must_use]
    pub(crate) const fn new(embedding: Vec<f32>, delay: Duration) -> Self {
        Self { embedding, delay }
    }
}

#[derive(Default)]
pub(crate) struct ScriptedEmbedding {
    rules: Mutex<HashMap<String, ScriptedRule>>,
}

impl ScriptedEmbedding {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set_rule<T: Into<String>>(&self, text: T, rule: ScriptedRule) {
        self.rules.lock().insert(text.into(), rule);
    }

    async fn embed_impl(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        let rule = self.rules.lock().get(text).cloned();
        if let Some(rule) = rule {
            if !rule.delay.is_zero() {
                tokio::time::sleep(rule.delay).await;
            }
            return Ok(rule.embedding);
        }
        DeterministicEmbedding.embed(text).await
    }
}

impl EmbeddingProvider for ScriptedEmbedding {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        Box::pin(self.embed_impl(text))
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Embedding provider that delegates to `DeterministicEmbedding` when enabled,
/// and returns `EmbeddingError::Disabled` when disabled.
pub(crate) struct ToggleableEmbedding {
    inner: DeterministicEmbedding,
    enabled: Arc<std::sync::atomic::AtomicBool>,
}

impl ToggleableEmbedding {
    pub(crate) fn new(initially_enabled: bool) -> (Self, Arc<std::sync::atomic::AtomicBool>) {
        let flag = Arc::new(std::sync::atomic::AtomicBool::new(initially_enabled));
        let provider = Self {
            inner: DeterministicEmbedding,
            enabled: Arc::clone(&flag),
        };
        (provider, flag)
    }
}

impl EmbeddingProvider for ToggleableEmbedding {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        Box::pin(async move {
            if self.enabled.load(std::sync::atomic::Ordering::Relaxed) {
                self.inner.embed(text).await
            } else {
                Err(EmbeddingError::Disabled)
            }
        })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
        // Always pass health check so memory_reembed doesn't reject at the gate
        Box::pin(async { Ok(()) })
    }
}

/// The kind of error a [`FailingEmbedding`] returns on every `embed` call.
pub(crate) enum FailingEmbeddingKind {
    /// Simulates a transient provider error (e.g. timeout, network).
    Transient,
    /// Simulates a permanent provider error (e.g. bad model).
    Permanent,
}

/// Embedding provider that always returns a configurable error.
/// Useful for testing graceful degradation and error handling paths.
pub(crate) struct FailingEmbedding {
    error_kind: FailingEmbeddingKind,
}

impl FailingEmbedding {
    /// Create a provider that always returns [`EmbeddingError::Transient`].
    #[must_use]
    pub(crate) const fn provider() -> Self {
        Self {
            error_kind: FailingEmbeddingKind::Transient,
        }
    }

    /// Create a provider that always returns [`EmbeddingError::Permanent`].
    #[must_use]
    pub(crate) const fn unavailable() -> Self {
        Self {
            error_kind: FailingEmbeddingKind::Permanent,
        }
    }
}

impl EmbeddingProvider for FailingEmbedding {
    fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        Box::pin(async move {
            match self.error_kind {
                FailingEmbeddingKind::Transient => Err(EmbeddingError::Transient("test transient error".into())),
                FailingEmbeddingKind::Permanent => Err(EmbeddingError::Permanent("test permanent error".into())),
            }
        })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
        // Health check passes so `memory_reembed` doesn't reject at the gate.
        Box::pin(async { Ok(()) })
    }
}

/// Embedding provider that delegates to [`DeterministicEmbedding`] after a
/// configurable delay. Useful for stress-testing the embedding orchestrator
/// under load where embeddings take non-trivial time.
pub(crate) struct SlowDeterministicEmbedding {
    delay: Duration,
}

impl SlowDeterministicEmbedding {
    #[must_use]
    pub(crate) const fn new(delay: Duration) -> Self {
        Self { delay }
    }
}

impl EmbeddingProvider for SlowDeterministicEmbedding {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            Ok(deterministic_embed(text))
        })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
        Box::pin(async { Ok(()) })
    }
}

pub(crate) fn sparse_embedding(entries: &[(usize, f32)]) -> Vec<f32> {
    let mut emb = vec![0.0_f32; 768];
    for &(idx, val) in entries {
        assert!(idx < emb.len(), "embedding index out of range: {idx}");
        emb[idx] = val;
    }
    emb
}

/// Spawn an MCP server with a caller-supplied embedding provider.
/// Returns `(client, server_ref)` — the server ref can be used for shutdown.
pub(crate) async fn setup_server_with(embedding: Arc<dyn EmbeddingProvider>) -> (RunningService<rmcp::RoleClient, ()>, RecallServer) {
    let store = SqliteStore::in_memory().unwrap();
    setup_server_with_store(store, embedding).await
}

/// Spawn an MCP server with a caller-supplied SQLite store and embedding provider.
/// Returns `(client, server_ref)` — the server ref can be used for shutdown.
pub(crate) async fn setup_server_with_store(store: SqliteStore, embedding: Arc<dyn EmbeddingProvider>) -> (RunningService<rmcp::RoleClient, ()>, RecallServer) {
    let server = RecallServer::new(store, embedding, LimitsConfig::default(), SearchConfig::default()).with_admin_tools();
    let server_ref = server.clone();

    let (server_transport, client_transport) = tokio::io::duplex(4096);

    tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });

    let client = ().serve(client_transport).await.unwrap();
    (client, server_ref)
}

/// Spawn an MCP server with caller-supplied embedding provider and limits.
/// Returns `(client, server_ref)`.
pub(crate) async fn setup_server_with_limits(embedding: Arc<dyn EmbeddingProvider>, limits: LimitsConfig) -> (RunningService<rmcp::RoleClient, ()>, RecallServer) {
    let store = SqliteStore::in_memory().unwrap();
    let server = RecallServer::new(store, embedding, limits, SearchConfig::default()).with_admin_tools();
    let server_ref = server.clone();

    let (server_transport, client_transport) = tokio::io::duplex(4096);

    tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });

    let client = ().serve(client_transport).await.unwrap();
    (client, server_ref)
}

/// Spawn an MCP server with explicit v2 authorization settings.
/// Returns `(client, server_ref)`.
pub(crate) async fn setup_server_with_auth(
    embedding: Arc<dyn EmbeddingProvider>,
    principal: Option<&str>,
    anonymous_policy: AnonymousPolicy,
) -> (RunningService<rmcp::RoleClient, ()>, RecallServer) {
    let store = SqliteStore::in_memory().unwrap();
    let engine = RecallEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine_with_auth(engine, principal.map(ToOwned::to_owned), anonymous_policy).with_admin_tools();
    let server_ref = server.clone();

    let (server_transport, client_transport) = tokio::io::duplex(4096);

    tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });

    let client = ().serve(client_transport).await.unwrap();
    (client, server_ref)
}

/// Coerce a concrete `Arc<C>` into `Arc<dyn Clock>` without triggering lint violations.
fn into_dyn_clock<C: Clock>(clock: Arc<C>) -> Arc<dyn Clock> {
    clock
}

/// Spawn an MCP server with a caller-supplied embedding provider and clock.
/// Returns `(client, server_ref)`.
pub(crate) async fn setup_server_with_clock(embedding: Arc<dyn EmbeddingProvider>, clock: Arc<dyn Clock>) -> (RunningService<rmcp::RoleClient, ()>, RecallServer) {
    let store = SqliteStore::in_memory_with_clock(Arc::clone(&clock)).unwrap();
    let server = RecallServer::new_with_clock(store, embedding, LimitsConfig::default(), SearchConfig::default(), clock).with_admin_tools();
    let server_ref = server.clone();

    let (server_transport, client_transport) = tokio::io::duplex(4096);

    tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });

    let client = ().serve(client_transport).await.unwrap();
    (client, server_ref)
}

/// Spawn an MCP server with `NoopEmbedding` (text search fallback mode).
/// Returns the connected client handle.
pub(crate) async fn setup_noop_server() -> RunningService<rmcp::RoleClient, ()> {
    let (client, _server) = setup_server_with(Arc::new(NoopEmbedding::new())).await;
    client
}

/// Spawn an MCP server with `NoopEmbedding` and caller-supplied limits.
pub(crate) async fn setup_noop_server_with_limits(limits: LimitsConfig) -> (RunningService<rmcp::RoleClient, ()>, RecallServer) {
    setup_server_with_limits(Arc::new(NoopEmbedding::new()), limits).await
}

/// Spawn an MCP server with `NoopEmbedding` and explicit v2 authorization settings.
pub(crate) async fn setup_noop_server_with_auth(principal: Option<&str>, anonymous_policy: AnonymousPolicy) -> RunningService<rmcp::RoleClient, ()> {
    let (client, _server) = setup_server_with_auth(Arc::new(NoopEmbedding::new()), principal, anonymous_policy).await;
    client
}

/// Spawn an MCP server with `NoopEmbedding` and a custom clock.
/// Returns `(client, server_ref)`.
pub(crate) async fn setup_noop_server_with_clock<C: Clock>(clock: Arc<C>) -> (RunningService<rmcp::RoleClient, ()>, RecallServer) {
    setup_server_with_clock(Arc::new(NoopEmbedding::new()), into_dyn_clock(clock)).await
}

#[derive(Debug)]
pub(crate) struct LegacySeed {
    content: String,
    tags: Vec<String>,
    source_agent: Option<String>,
    source_conversation: Option<String>,
}

impl LegacySeed {
    pub(crate) fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            tags: Vec::new(),
            source_agent: None,
            source_conversation: None,
        }
    }

    pub(crate) fn tags(mut self, tags: &[&str]) -> Self {
        self.tags = tags.iter().map(ToString::to_string).collect();
        self
    }

    pub(crate) fn source_agent(mut self, source_agent: &str) -> Self {
        self.source_agent = Some(source_agent.to_owned());
        self
    }

    pub(crate) fn source_conversation(mut self, source_conversation: &str) -> Self {
        self.source_conversation = Some(source_conversation.to_owned());
        self
    }
}

pub(crate) async fn setup_noop_server_with_legacy_memories(seeds: Vec<LegacySeed>) -> (RunningService<rmcp::RoleClient, ()>, Vec<MemoryId>) {
    setup_noop_server_with_auth_and_legacy_memories(seeds, Some("stdio"), AnonymousPolicy::PublicReadOnly).await
}

pub(crate) async fn setup_noop_server_with_auth_and_legacy_memories(
    seeds: Vec<LegacySeed>,
    principal: Option<&str>,
    anonymous_policy: AnonymousPolicy,
) -> (RunningService<rmcp::RoleClient, ()>, Vec<MemoryId>) {
    let store = SqliteStore::in_memory().unwrap();
    let mut ids = Vec::with_capacity(seeds.len());
    for seed in seeds {
        let provenance = Provenance::new_for_test(seed.source_agent, seed.source_conversation.clone(), seed.source_conversation);
        let memory = Memory::new_for_test(seed.content, seed.tags, provenance, AccessPolicy::Public);
        let id = store.store(&memory, None).await.unwrap();
        ids.push(id);
    }

    let engine = RecallEngine::new(store, Arc::new(NoopEmbedding::new()), LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine_with_auth(engine, principal.map(ToOwned::to_owned), anonymous_policy).with_admin_tools();
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });
    let client = ().serve(client_transport).await.unwrap();
    (client, ids)
}

/// Spawn an MCP server with `DeterministicEmbedding` (semantic search mode).
/// Returns `(client, server_ref)` — the server ref can be used for shutdown.
pub(crate) async fn setup_embedding_server() -> (RunningService<rmcp::RoleClient, ()>, RecallServer) {
    setup_server_with(Arc::new(DeterministicEmbedding)).await
}

/// Call a tool by name with JSON arguments. Asserts success and deserializes the response.
#[expect(clippy::panic, reason = "test helper should fail loudly for non-object JSON arguments")]
pub(crate) fn call_tool_params(name: &str, args: serde_json::Value) -> CallToolRequestParams {
    let serde_json::Value::Object(args) = args else {
        panic!("tool args must be a JSON object");
    };
    CallToolRequestParams::new(name.to_owned()).with_arguments(args)
}

/// Call a tool by name with JSON arguments. Asserts success and deserializes the response.
pub(crate) async fn call_tool<T: DeserializeOwned>(client: &RunningService<rmcp::RoleClient, ()>, name: &str, args: serde_json::Value) -> T {
    let result = client.call_tool(call_tool_params(name, args)).await.unwrap();
    assert!(!result.is_error.unwrap_or(false), "tool {name} returned error: {}", extract_text(&result));
    let text = extract_text(&result);
    #[expect(clippy::panic, reason = "test helper: panic with diagnostic context on deserialization failure")]
    serde_json::from_str(text).unwrap_or_else(|e| panic!("failed to parse response from {name}: {e}\nraw: {text}"))
}

/// Call a tool expecting an application-level error (`is_error: true`). Returns the error text.
pub(crate) async fn call_tool_error(client: &RunningService<rmcp::RoleClient, ()>, name: &str, args: serde_json::Value) -> String {
    let result = client.call_tool(call_tool_params(name, args)).await.unwrap();
    assert!(result.is_error.unwrap_or(false), "expected error from {name}, got success");
    extract_text(&result).to_owned()
}

/// Assert a call fails with invalid params and the error mentions a specific fragment.
///
/// Some transports surface invalid params as a protocol `Err(...)`, while others
/// return an application-level `CallToolResult` with `is_error: true`. This helper
/// accepts either shape and validates the message content.
pub(crate) async fn assert_invalid_params_contains(client: &RunningService<rmcp::RoleClient, ()>, name: &str, args: serde_json::Value, expected_fragment: &str) {
    let result = client.call_tool(call_tool_params(name, args)).await;

    match result {
        Ok(call_result) => {
            assert!(call_result.is_error.unwrap_or(false), "expected error from {name}, got success");
            let text = extract_text(&call_result);
            assert!(text.contains(expected_fragment), "error did not contain `{expected_fragment}`: {text}");
        }
        Err(err) => {
            let text = err.to_string();
            assert!(text.contains(expected_fragment), "error did not contain `{expected_fragment}`: {text}");
        }
    }
}

fn extract_text(result: &CallToolResult) -> &str {
    assert!(!result.content.is_empty(), "MCP result has no content items");
    let Some(t) = result.content[0].as_text() else {
        #[expect(clippy::panic, reason = "test helper: unreachable unless MCP response format changes")]
        {
            panic!("expected text content, got: {:?}", result.content[0])
        }
    };
    &t.text
}

// ---------------------------------------------------------------------------
// HTTP transport helpers
// ---------------------------------------------------------------------------

/// Spawn an HTTP MCP server with a caller-supplied embedding provider.
/// Returns `(url, cancellation_token, server_ref)`.
pub(crate) async fn spawn_http_server_with(embedding: Arc<dyn EmbeddingProvider>) -> (String, CancellationToken, RecallServer) {
    let store = SqliteStore::in_memory().unwrap();
    let engine = RecallEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine_with_auth_and_http(
        engine,
        None,
        AnonymousPolicy::PublicReadOnly,
        Some(TEST_HTTP_AUTH_TOKEN.to_owned()),
        HttpPrincipalSource::fixed(TEST_HTTP_PRINCIPAL),
    );
    spawn_http_server_inner(server, localhold::config::DEFAULT_HTTP_MAX_BODY_BYTES, Some(TEST_HTTP_AUTH_TOKEN), None).await
}

/// Spawn an HTTP MCP server with `NoopEmbedding` and a custom request body limit.
/// Returns `(url, cancellation_token, server_ref)`.
pub(crate) async fn spawn_http_noop_server_with_body_limit(max_body_bytes: usize) -> (String, CancellationToken, RecallServer) {
    let store = SqliteStore::in_memory().unwrap();
    let engine = RecallEngine::new(store, Arc::new(NoopEmbedding::new()), LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine_with_auth_and_http(engine, None, AnonymousPolicy::PublicReadWrite, None, HttpPrincipalSource::fixed(TEST_HTTP_PRINCIPAL));
    spawn_http_server_inner(server, max_body_bytes, None, None).await
}

/// Spawn an HTTP MCP server with a custom DNS-rebinding host allowlist.
pub(crate) async fn spawn_http_noop_server_with_allowed_hosts(http_allowed_hosts: Vec<String>) -> (String, CancellationToken, RecallServer) {
    let store = SqliteStore::in_memory().unwrap();
    let engine = RecallEngine::new(store, Arc::new(NoopEmbedding::new()), LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine_with_auth_and_http(engine, None, AnonymousPolicy::PublicReadWrite, None, HttpPrincipalSource::fixed(TEST_HTTP_PRINCIPAL));
    spawn_http_server_inner(server, localhold::config::DEFAULT_HTTP_MAX_BODY_BYTES, None, Some(http_allowed_hosts)).await
}

/// Spawn an HTTP MCP server with explicit v2 authorization settings.
/// Returns `(url, cancellation_token, server_ref)`.
pub(crate) async fn spawn_http_server_with_auth(
    embedding: Arc<dyn EmbeddingProvider>,
    principal: Option<&str>,
    anonymous_policy: AnonymousPolicy,
    http_auth_token: Option<&str>,
) -> (String, CancellationToken, RecallServer) {
    let store = SqliteStore::in_memory().unwrap();
    let engine = RecallEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine_with_auth_and_http(
        engine,
        principal.map(ToOwned::to_owned),
        anonymous_policy,
        http_auth_token.map(ToOwned::to_owned),
        HttpPrincipalSource::fixed(TEST_HTTP_PRINCIPAL),
    );
    spawn_http_server_inner(server, localhold::config::DEFAULT_HTTP_MAX_BODY_BYTES, http_auth_token, None).await
}

/// Spawn an HTTP MCP server that trusts a proxy-only identity header.
///
/// Tests using this helper model a deployment where clients cannot bypass the
/// proxy and the proxy strips client-supplied copies of the principal header.
pub(crate) async fn spawn_http_server_with_trusted_proxy_auth(
    embedding: Arc<dyn EmbeddingProvider>,
    principal: Option<&str>,
    anonymous_policy: AnonymousPolicy,
    http_auth_token: &str,
) -> (String, CancellationToken, RecallServer) {
    let store = SqliteStore::in_memory().unwrap();
    let engine = RecallEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine_with_auth_and_http(
        engine,
        principal.map(ToOwned::to_owned),
        anonymous_policy,
        Some(http_auth_token.to_owned()),
        HttpPrincipalSource::trusted_proxy_header(DEFAULT_HTTP_PRINCIPAL_HEADER),
    );
    spawn_http_server_inner(server, localhold::config::DEFAULT_HTTP_MAX_BODY_BYTES, Some(http_auth_token), None).await
}

/// Spawn an HTTP MCP server with a caller-supplied embedding provider and clock.
/// Returns `(url, cancellation_token, server_ref)`.
pub(crate) async fn spawn_http_server_with_clock(embedding: Arc<dyn EmbeddingProvider>, clock: Arc<dyn Clock>) -> (String, CancellationToken, RecallServer) {
    let store = SqliteStore::in_memory_with_clock(Arc::clone(&clock)).unwrap();
    let engine = RecallEngine::new_with_clock(store, embedding, LimitsConfig::default(), SearchConfig::default(), clock);
    let server = RecallServer::from_engine_with_auth_and_http(
        engine,
        None,
        AnonymousPolicy::PublicReadOnly,
        Some(TEST_HTTP_AUTH_TOKEN.to_owned()),
        HttpPrincipalSource::fixed(TEST_HTTP_PRINCIPAL),
    );
    spawn_http_server_inner(server, localhold::config::DEFAULT_HTTP_MAX_BODY_BYTES, Some(TEST_HTTP_AUTH_TOKEN), None).await
}

async fn spawn_http_server_inner(
    server: RecallServer,
    max_body_bytes: usize,
    http_auth_token: Option<&str>,
    http_allowed_hosts: Option<Vec<String>>,
) -> (String, CancellationToken, RecallServer) {
    let server_ref = server.clone();

    let ct = CancellationToken::new();
    let mut config = ServerConfig::default();
    config.max_body_bytes = max_body_bytes;
    config.admin_tools_enabled = true;
    config.http_auth_token = http_auth_token.map(ToOwned::to_owned);
    if let Some(http_allowed_hosts) = http_allowed_hosts {
        config.http_allowed_hosts = http_allowed_hosts;
    }
    let router = build_router(server, &config, &ct).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let shutdown_ct = ct.clone();
    tokio::spawn(async move {
        let _serve = axum::serve(listener, router).with_graceful_shutdown(async move { shutdown_ct.cancelled().await }).await;
    });

    (format!("http://{addr}/mcp"), ct, server_ref)
}

/// Connect an MCP client to the HTTP server at the given URL.
pub(crate) async fn connect_http_client(url: &str) -> RunningService<rmcp::RoleClient, ()> {
    connect_http_client_with_bearer(url, TEST_HTTP_AUTH_TOKEN).await
}

/// Connect an MCP client without HTTP authorization headers.
pub(crate) async fn connect_http_client_unauthenticated(url: &str) -> RunningService<rmcp::RoleClient, ()> {
    let transport = StreamableHttpClientTransport::from_uri(url);
    ().serve(transport).await.unwrap()
}

/// Connect an MCP client with bearer auth and no caller-controlled identity.
pub(crate) async fn connect_http_client_with_bearer(url: &str, token: &str) -> RunningService<rmcp::RoleClient, ()> {
    let config = StreamableHttpClientTransportConfig::with_uri(url).auth_header(token);
    let transport = StreamableHttpClientTransport::from_config(config);
    ().serve(transport).await.unwrap()
}

/// Connect an MCP client with bearer auth and a principal header.
///
/// Fixed-identity servers ignore this header. Trusted-proxy tests use it to
/// model the identity header after the proxy has authenticated the caller.
pub(crate) async fn connect_http_client_with_auth(url: &str, token: &str, principal: &str) -> RunningService<rmcp::RoleClient, ()> {
    let mut headers = HashMap::new();
    headers.insert(HeaderName::from_static(DEFAULT_HTTP_PRINCIPAL_HEADER), HeaderValue::from_str(principal).unwrap());
    let config = StreamableHttpClientTransportConfig::with_uri(url).auth_header(token).custom_headers(headers);
    let transport = StreamableHttpClientTransport::from_config(config);
    ().serve(transport).await.unwrap()
}

/// Spawn an HTTP MCP server with `NoopEmbedding` (text search fallback mode).
pub(crate) async fn setup_http_noop_server() -> (String, CancellationToken, RecallServer) {
    spawn_http_server_with(Arc::new(NoopEmbedding::new())).await
}

/// Spawn an HTTP MCP server with `NoopEmbedding` and explicit v2 auth settings.
pub(crate) async fn setup_http_noop_server_with_auth(
    principal: Option<&str>,
    anonymous_policy: AnonymousPolicy,
    http_auth_token: Option<&str>,
) -> (String, CancellationToken, RecallServer) {
    spawn_http_server_with_auth(Arc::new(NoopEmbedding::new()), principal, anonymous_policy, http_auth_token).await
}

/// Spawn an HTTP MCP server with `NoopEmbedding` behind a modeled trusted proxy.
pub(crate) async fn setup_http_noop_server_with_trusted_proxy_auth(
    principal: Option<&str>,
    anonymous_policy: AnonymousPolicy,
    http_auth_token: &str,
) -> (String, CancellationToken, RecallServer) {
    spawn_http_server_with_trusted_proxy_auth(Arc::new(NoopEmbedding::new()), principal, anonymous_policy, http_auth_token).await
}

/// Spawn an HTTP MCP server with `NoopEmbedding` and a custom clock.
pub(crate) async fn setup_http_noop_server_with_clock<C: Clock>(clock: Arc<C>) -> (String, CancellationToken, RecallServer) {
    spawn_http_server_with_clock(Arc::new(NoopEmbedding::new()), into_dyn_clock(clock)).await
}

/// Spawn an HTTP MCP server with `DeterministicEmbedding` (semantic search mode).
pub(crate) async fn setup_http_embedding_server() -> (String, CancellationToken, RecallServer) {
    spawn_http_server_with(Arc::new(DeterministicEmbedding)).await
}

// ---------------------------------------------------------------------------
// TestHarness — transport-agnostic test harness
// ---------------------------------------------------------------------------

/// Transport-agnostic test harness that wraps an MCP client, an optional
/// server reference, and an optional HTTP cancellation token.  Test bodies
/// that work against a `TestHarness` can be stamped out for *both* stdio and
/// HTTP transports via the [`transport_test!`] macro.
pub(crate) struct TestHarness {
    client: RunningService<rmcp::RoleClient, ()>,
    server: Option<RecallServer>,
    cancellation_token: Option<CancellationToken>,
}

impl TestHarness {
    // -- constructors -------------------------------------------------------

    /// Stdio transport with `NoopEmbedding`.
    pub(crate) async fn stdio_noop() -> Self {
        let client = setup_noop_server().await;
        Self {
            client,
            server: None,
            cancellation_token: None,
        }
    }

    /// Stdio transport with `DeterministicEmbedding`.
    pub(crate) async fn stdio_embedding() -> Self {
        let (client, server) = setup_embedding_server().await;
        Self {
            client,
            server: Some(server),
            cancellation_token: None,
        }
    }

    /// Stdio transport with `NoopEmbedding` and a caller-supplied clock.
    pub(crate) async fn stdio_noop_clock<C: Clock>(clock: Arc<C>) -> Self {
        let (client, server) = setup_noop_server_with_clock(clock).await;
        Self {
            client,
            server: Some(server),
            cancellation_token: None,
        }
    }

    /// HTTP transport with `NoopEmbedding`.
    pub(crate) async fn http_noop() -> Self {
        let (url, ct, server) = setup_http_noop_server().await;
        let client = connect_http_client(&url).await;
        Self {
            client,
            server: Some(server),
            cancellation_token: Some(ct),
        }
    }

    /// HTTP transport with `DeterministicEmbedding`.
    pub(crate) async fn http_embedding() -> Self {
        let (url, ct, server) = setup_http_embedding_server().await;
        let client = connect_http_client(&url).await;
        Self {
            client,
            server: Some(server),
            cancellation_token: Some(ct),
        }
    }

    /// HTTP transport with `NoopEmbedding` and a caller-supplied clock.
    pub(crate) async fn http_noop_clock<C: Clock>(clock: Arc<C>) -> Self {
        let (url, ct, server) = setup_http_noop_server_with_clock(clock).await;
        let client = connect_http_client(&url).await;
        Self {
            client,
            server: Some(server),
            cancellation_token: Some(ct),
        }
    }

    // -- accessors ----------------------------------------------------------

    /// Borrow the MCP client.
    pub(crate) const fn client(&self) -> &RunningService<rmcp::RoleClient, ()> {
        &self.client
    }

    /// Borrow the server reference; panics if unavailable.
    #[expect(clippy::panic, reason = "test helper: server must be present for tests that need it")]
    #[expect(clippy::option_if_let_else, reason = "map_or_else with panic closure is less readable than match")]
    #[expect(clippy::ref_patterns, reason = "match ergonomics: ref is clearer than & in this pattern")]
    pub(crate) fn server(&self) -> &RecallServer {
        match self.server {
            Some(ref s) => s,
            None => panic!("TestHarness was created without a server reference"),
        }
    }

    // -- cleanup ------------------------------------------------------------

    /// Shut down gracefully (cancel HTTP listener if applicable, then
    /// call `server.shutdown()`).
    pub(crate) async fn shutdown(self) {
        if let Some(ct) = &self.cancellation_token {
            ct.cancel();
        }
        if let Some(server) = &self.server {
            server.shutdown().await;
        }
    }
}

/// Generate a pair of `#[tokio::test]` functions — one for stdio and one for
/// HTTP — from a single async test body that receives a [`TestHarness`].
///
/// # Variants
///
/// ```ignore
/// transport_test!(noop, test_name, |harness| async move { ... });
/// transport_test!(embedding, test_name, |harness| async move { ... });
/// transport_test!(noop_clock, test_name, |harness, clock| async move { ... });
/// ```
///
/// Outer attributes (e.g. `#[expect(...)]`) can be placed before the
/// `transport_test!` call by passing them as the first argument:
///
/// ```ignore
/// transport_test!(#[expect(clippy::too_many_lines, reason = "...")], noop, name, |h| async move { ... });
/// ```
///
/// The first token selects the constructor pair on `TestHarness`.
macro_rules! transport_test {
    // -- noop (no clock) ---------------------------------------------------
    ($(#[$attr:meta]),* , noop, $name:ident, |$h:ident| async move $body:block) => {
        pastey::paste! {
            $(#[$attr])*
            #[tokio::test]
            async fn [< stdio_ $name >]() {
                let $h = super::helpers::TestHarness::stdio_noop().await;
                $body
            }

            $(#[$attr])*
            #[tokio::test]
            async fn [< http_ $name >]() {
                let $h = super::helpers::TestHarness::http_noop().await;
                $body
            }
        }
    };
    (noop, $name:ident, |$h:ident| async move $body:block) => {
        super::helpers::transport_test!(, noop, $name, |$h| async move $body);
    };

    // -- embedding (no clock) -----------------------------------------------
    ($(#[$attr:meta]),* , embedding, $name:ident, |$h:ident| async move $body:block) => {
        pastey::paste! {
            $(#[$attr])*
            #[tokio::test]
            async fn [< stdio_ $name >]() {
                let $h = super::helpers::TestHarness::stdio_embedding().await;
                $body
            }

            $(#[$attr])*
            #[tokio::test]
            async fn [< http_ $name >]() {
                let $h = super::helpers::TestHarness::http_embedding().await;
                $body
            }
        }
    };
    (embedding, $name:ident, |$h:ident| async move $body:block) => {
        super::helpers::transport_test!(, embedding, $name, |$h| async move $body);
    };

    // -- noop + clock -------------------------------------------------------
    ($(#[$attr:meta]),* , noop_clock, $name:ident, |$h:ident, $clock:ident| async move $body:block) => {
        pastey::paste! {
            $(#[$attr])*
            #[tokio::test]
            async fn [< stdio_ $name >]() {
                let $clock = std::sync::Arc::new(localhold::clock::MockClock::new());
                let $h = super::helpers::TestHarness::stdio_noop_clock(std::sync::Arc::clone(&$clock)).await;
                $body
            }

            $(#[$attr])*
            #[tokio::test]
            async fn [< http_ $name >]() {
                let $clock = std::sync::Arc::new(localhold::clock::MockClock::new());
                let $h = super::helpers::TestHarness::http_noop_clock(std::sync::Arc::clone(&$clock)).await;
                $body
            }
        }
    };
    (noop_clock, $name:ident, |$h:ident, $clock:ident| async move $body:block) => {
        super::helpers::transport_test!(, noop_clock, $name, |$h, $clock| async move $body);
    };
}

pub(crate) use transport_test;
