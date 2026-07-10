//! HTTP-transport-specific integration tests.
//!
//! Tests that exercise multi-session behaviour, concurrent access, and
//! session disconnect resilience live here. Tests that are semantically
//! identical across stdio and HTTP have been moved to `transport_matrix.rs`.

use std::time::Duration;

use localhold::{
    config::AnonymousPolicy,
    server::params::{AdminListResponse, DeleteResponse, HistoryResponse, ReadResponse, RecallResponse, RememberManyResponse, RememberResponse, UpdateResponse},
    types::{AuditAction, SearchMode},
};
use reqwest::{
    RequestBuilder, StatusCode,
    header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HOST, WWW_AUTHENTICATE},
};
use serde_json::json;

use super::helpers::{
    TEST_HTTP_AUTH_TOKEN, TEST_HTTP_PRINCIPAL, await_embeddings, call_tool, call_tool_error, connect_http_client, connect_http_client_unauthenticated,
    connect_http_client_with_auth, connect_http_client_with_bearer, setup_http_embedding_server, setup_http_noop_server, setup_http_noop_server_with_auth,
    setup_http_noop_server_with_trusted_proxy_auth, spawn_http_noop_server_with_allowed_hosts, spawn_http_noop_server_with_body_limit,
};

// ===========================================================================
// HTTP smoke tests (transport-specific behaviour)
// ===========================================================================

/// Number of MCP tools the server is expected to expose.
const EXPECTED_TOOL_COUNT: usize = 22;

const RAW_INITIALIZE: &str =
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"localhold-http-test","version":"1"}}}"#;

fn raw_mcp_post(url: &str, body: impl Into<reqwest::Body>) -> RequestBuilder {
    reqwest::Client::new()
        .post(url)
        .header(ACCEPT, "application/json, text/event-stream")
        .header(CONTENT_TYPE, "application/json")
        .body(body)
}

fn raw_authenticated_mcp_post(url: &str, body: impl Into<reqwest::Body>) -> RequestBuilder {
    raw_mcp_post(url, body)
        .header(AUTHORIZATION, format!("Bearer {TEST_HTTP_AUTH_TOKEN}"))
        .header(localhold::config::DEFAULT_HTTP_PRINCIPAL_HEADER, TEST_HTTP_PRINCIPAL)
}

#[tokio::test]
async fn http_initialize_and_tool_call() {
    let (url, ct, server) = setup_http_noop_server().await;
    let client = connect_http_client(&url).await;

    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), EXPECTED_TOOL_COUNT);
    let names: std::collections::BTreeSet<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    for expected in [
        "brief",
        "recall",
        "read",
        "read_many",
        "remember",
        "remember_many",
        "handoff",
        "admin_list",
        "admin_scope_register",
        "admin_v2_migrate_metadata",
        "admin_count",
    ] {
        assert!(names.contains(expected), "HTTP tool list should include {expected}");
    }

    let remembered: RememberResponse = call_tool(&client, "remember", json!({"content": "hello from http"})).await;
    assert!(!remembered.id.to_string().is_empty());

    let read: ReadResponse = call_tool(&client, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.memory.content, "hello from http");

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_multiple_sessions_share_data() {
    let (url, ct, server) = setup_http_noop_server().await;

    let client_a = connect_http_client(&url).await;
    let remembered: RememberResponse = call_tool(&client_a, "remember", json!({"content": "shared data"})).await;

    let client_b = connect_http_client(&url).await;
    let read: ReadResponse = call_tool(&client_b, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.memory.content, "shared data");

    let list_a: AdminListResponse = call_tool(&client_a, "admin_list", json!({})).await;
    let list_b: AdminListResponse = call_tool(&client_b, "admin_list", json!({})).await;
    assert_eq!(list_a.count, 1);
    assert_eq!(list_b.count, 1);

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_v2_fixed_bearer_principal_enables_write_without_launch_principal() {
    let (url, ct, server) = setup_http_noop_server_with_auth(None, AnonymousPolicy::PublicReadOnly, Some("secret-token")).await;

    let unauthenticated = raw_mcp_post(&url, RAW_INITIALIZE).send().await.unwrap();
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(unauthenticated.headers().get(WWW_AUTHENTICATE).unwrap(), "Bearer");

    let authenticated_client = connect_http_client_with_bearer(&url, "secret-token").await;
    let remembered: RememberResponse = call_tool(
        &authenticated_client,
        "remember",
        json!({"content": "trusted http write", "scope": "gearboxlogic/localhold"}),
    )
    .await;
    let read: ReadResponse = call_tool(&authenticated_client, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.agent_label.as_deref(), Some(TEST_HTTP_PRINCIPAL));
    assert_eq!(read.scope.as_deref(), Some("gearboxlogic/localhold"));

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_without_auth_does_not_inherit_launch_principal() {
    let (url, ct, server) = setup_http_noop_server_with_auth(Some("launch"), AnonymousPolicy::PublicReadOnly, None).await;

    let client = connect_http_client_unauthenticated(&url).await;
    let error = call_tool_error(&client, "remember", json!({"content": "must not write as launch"})).await;
    assert!(error.contains("anonymous writes are disabled"), "expected anonymous write denial, got: {error}");

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_v2_migration_tools_require_local_admin_context() {
    let (url, ct, server) = setup_http_noop_server_with_auth(Some("launch"), AnonymousPolicy::PublicReadOnly, Some("secret-token")).await;
    let client = connect_http_client_with_auth(&url, "secret-token", "alice").await;

    let report_err = call_tool_error(&client, "admin_v2_migration_report", json!({})).await;
    assert!(report_err.contains("local server admin"), "expected local-admin denial, got: {report_err}");

    let migrate_err = call_tool_error(&client, "admin_v2_migrate_metadata", json!({"dry_run": true})).await;
    assert!(migrate_err.contains("local server admin"), "expected local-admin denial, got: {migrate_err}");

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_v2_forged_principal_header_cannot_override_fixed_identity() {
    let (url, ct, server) = setup_http_noop_server_with_auth(Some("launch"), AnonymousPolicy::PublicReadOnly, Some("secret-token")).await;

    let client = connect_http_client_with_auth(&url, "secret-token", "alice").await;
    let remembered: RememberResponse = call_tool(&client, "remember", json!({"content": "trusted http override"})).await;
    let read: ReadResponse = call_tool(&client, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.agent_label.as_deref(), Some(TEST_HTTP_PRINCIPAL));
    assert_eq!(read.created_by_principal.as_deref(), Some(TEST_HTTP_PRINCIPAL));
    assert_ne!(read.created_by_principal.as_deref(), Some("alice"));
    assert_ne!(read.created_by_principal.as_deref(), Some("launch"));

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_v2_bad_bearer_does_not_fall_back_to_launch_principal() {
    let (url, ct, server) = setup_http_noop_server_with_auth(Some("launch"), AnonymousPolicy::PublicReadOnly, Some("secret-token")).await;

    for authorization in ["Bearer wrong-token", "Basic secret-token", "Bearer"] {
        let response = raw_mcp_post(&url, RAW_INITIALIZE).header(AUTHORIZATION, authorization).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "authorization value should be rejected: {authorization}");
        assert_eq!(response.headers().get(WWW_AUTHENTICATE).unwrap(), "Bearer");
    }

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_endpoint_path_is_exact() {
    let (url, ct, server) = setup_http_noop_server().await;

    let response = raw_mcp_post(&format!("{url}/extra"), RAW_INITIALIZE).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_host_allowlist_supports_reverse_proxy_hosts() {
    let (url, ct, server) = spawn_http_noop_server_with_allowed_hosts(vec!["recall.internal".to_owned()]).await;

    let rejected = raw_mcp_post(&url, RAW_INITIALIZE).header(HOST, "other.internal").send().await.unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);

    let accepted = raw_mcp_post(&url, RAW_INITIALIZE).header(HOST, "recall.internal").send().await.unwrap();
    assert_eq!(accepted.status(), StatusCode::OK);

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_delete_closes_session_and_reuse_returns_not_found() {
    let (url, ct, server) = setup_http_noop_server().await;

    let initialized = raw_authenticated_mcp_post(&url, RAW_INITIALIZE).send().await.unwrap();
    assert_eq!(initialized.status(), StatusCode::OK);
    let session_id = initialized.headers().get("mcp-session-id").unwrap().to_str().unwrap().to_owned();

    let deleted = reqwest::Client::new()
        .delete(&url)
        .header(AUTHORIZATION, format!("Bearer {TEST_HTTP_AUTH_TOKEN}"))
        .header(localhold::config::DEFAULT_HTTP_PRINCIPAL_HEADER, TEST_HTTP_PRINCIPAL)
        .header("mcp-session-id", &session_id)
        .header("mcp-protocol-version", "2025-06-18")
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::ACCEPTED);

    let stale = raw_authenticated_mcp_post(&url, r#"{"jsonrpc":"2.0","id":2,"method":"ping","params":{}}"#)
        .header("mcp-session-id", session_id)
        .header("mcp-protocol-version", "2025-06-18")
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::NOT_FOUND);

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_deleted_memory_history_uses_authenticated_tombstone_authorization() {
    let (url, ct, server) = setup_http_noop_server_with_trusted_proxy_auth(None, AnonymousPolicy::PublicReadOnly, "secret-token").await;
    let owner = connect_http_client_with_auth(&url, "secret-token", "owner").await;

    let remembered: RememberResponse = call_tool(
        &owner,
        "remember",
        json!({
            "content": "http deleted restricted content",
            "access_policy": {"type": "restricted", "allowed": ["friend"]}
        }),
    )
    .await;
    let deleted: DeleteResponse = call_tool(&owner, "forget", json!({"id": remembered.id})).await;
    assert!(deleted.deleted);

    let friend = connect_http_client_with_auth(&url, "secret-token", "friend").await;
    let friend_history: HistoryResponse = call_tool(&friend, "admin_history", json!({"id": remembered.id})).await;
    assert!(
        friend_history
            .entries
            .iter()
            .any(|entry| entry.action == AuditAction::Delete && entry.principal.as_deref() == Some("owner")),
        "friend should see the delete audit entry through the tombstone"
    );

    let intruder = connect_http_client_with_auth(&url, "secret-token", "intruder").await;
    let intruder_history: HistoryResponse = call_tool(&intruder, "admin_history", json!({"id": remembered.id})).await;
    assert!(intruder_history.entries.is_empty());

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_rejects_request_body_larger_than_configured_limit() {
    let (url, ct, server) = spawn_http_noop_server_with_body_limit(128).await;
    let oversized_body = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{{"padding":"{}"}}}}"#, "x".repeat(512));

    let response = reqwest::Client::new()
        .post(&url)
        .header("content-type", "application/json")
        .body(oversized_body)
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    ct.cancel();
    server.shutdown().await;
}

// ===========================================================================
// HTTP-specific concurrency tests
// ===========================================================================

async fn remember_memories_for_client(url: &str, client_idx: i32) {
    let client = connect_http_client(url).await;
    for mem_idx in 0_i32..5_i32 {
        let _resp: RememberResponse = call_tool(&client, "remember", json!({"content": format!("client{client_idx}-mem{mem_idx}")})).await;
    }
}

#[tokio::test]
async fn http_concurrent_remembers_no_data_loss() {
    let (url, ct, server) = setup_http_noop_server().await;

    let mut handles = Vec::new();
    for client_idx in 0_i32..10_i32 {
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            remember_memories_for_client(&url, client_idx).await;
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let reader = connect_http_client(&url).await;
    let resp: AdminListResponse = call_tool(&reader, "admin_list", json!({"limit": 100_i32})).await;
    assert_eq!(resp.count, 50);

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_concurrent_remember_and_recall() {
    let (url, ct, server) = setup_http_noop_server().await;

    let writer = connect_http_client(&url).await;
    for i in 0_i32..5_i32 {
        let _resp: RememberResponse = call_tool(&writer, "remember", json!({"content": format!("seed memory {i}")})).await;
    }

    let writer_url = url.clone();
    let writer_handle = tokio::spawn(async move {
        let client = connect_http_client(&writer_url).await;
        for i in 5_i32..15_i32 {
            let _resp: RememberResponse = call_tool(&client, "remember", json!({"content": format!("concurrent memory {i}")})).await;
        }
    });

    let reader_url = url.clone();
    let reader_handle = tokio::spawn(async move {
        let client = connect_http_client(&reader_url).await;
        for _ in 0_i32..10_i32 {
            let resp: RecallResponse = call_tool(&client, "recall", json!({"query": "memory"})).await;
            assert!(resp.count >= 1);
        }
    });

    writer_handle.await.unwrap();
    reader_handle.await.unwrap();

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_session_disconnect_no_corruption() {
    let (url, ct, server) = setup_http_noop_server().await;

    let id = {
        let client_a = connect_http_client(&url).await;
        let remembered: RememberResponse = call_tool(&client_a, "remember", json!({"content": "before disconnect"})).await;
        remembered.id
    };

    let client_b = connect_http_client(&url).await;
    let read: ReadResponse = call_tool(&client_b, "read", json!({"id": id})).await;
    assert_eq!(read.memory.content, "before disconnect");

    let remembered: RememberResponse = call_tool(&client_b, "remember", json!({"content": "after disconnect"})).await;
    assert!(!remembered.id.to_string().is_empty());

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_concurrent_revisions_to_same_memory() {
    let (url, ct, server) = setup_http_noop_server().await;

    let creator = connect_http_client(&url).await;
    let remembered: RememberResponse = call_tool(&creator, "remember", json!({"content": "original"})).await;
    let id = remembered.id;

    let mut handles = Vec::new();
    for i in 0_i32..5_i32 {
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let client = connect_http_client(&url).await;
            let resp: UpdateResponse = call_tool(&client, "revise", json!({"id": id, "content": format!("update-{i}")})).await;
            assert!(resp.updated);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let read: ReadResponse = call_tool(&creator, "read", json!({"id": id})).await;
    assert!(
        read.memory.content.starts_with("update-"),
        "expected one of the concurrent updates, got: {}",
        read.memory.content
    );

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_interleaved_crud_across_sessions() {
    let (url, ct, server) = setup_http_noop_server().await;

    let client_a = connect_http_client(&url).await;
    let remembered: RememberResponse = call_tool(&client_a, "remember", json!({"content": "original from A", "tags": ["shared"]})).await;
    let id = remembered.id;

    let client_b = connect_http_client(&url).await;
    let update_resp: UpdateResponse = call_tool(&client_b, "revise", json!({"id": id, "content": "updated by B"})).await;
    assert!(update_resp.updated);

    let read: ReadResponse = call_tool(&client_a, "read", json!({"id": id})).await;
    assert_eq!(read.memory.content, "updated by B");

    let client_c = connect_http_client(&url).await;
    let del_resp: DeleteResponse = call_tool(&client_c, "forget", json!({"id": id})).await;
    assert!(del_resp.deleted);

    let err = call_tool_error(&client_a, "read", json!({"id": id})).await;
    assert!(err.contains("not found"));

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_concurrent_remember_many() {
    let (url, ct, server) = setup_http_noop_server().await;

    let mut handles = Vec::new();
    for batch_idx in 0_i32..5_i32 {
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let client = connect_http_client(&url).await;
            let items: Vec<_> = (0_i32..10_i32).map(|i| json!({"content": format!("batch{batch_idx}-item{i}")})).collect();
            let _resp: RememberManyResponse = call_tool(&client, "remember_many", json!({"memories": items})).await;
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    let reader = connect_http_client(&url).await;
    let listed: AdminListResponse = call_tool(&reader, "admin_list", json!({"limit": 200_i32})).await;
    assert_eq!(listed.count, 50, "5 batches * 10 items = 50");

    ct.cancel();
    server.shutdown().await;
}

#[tokio::test]
async fn http_semantic_recall_concurrent_with_embeddings() {
    let (url, ct, server) = setup_http_embedding_server().await;
    let client = connect_http_client(&url).await;

    let _rust: RememberResponse = call_tool(&client, "remember", json!({"content": "Rust programming language"})).await;
    let _cooking: RememberResponse = call_tool(&client, "remember", json!({"content": "cooking pasta recipe"})).await;

    await_embeddings(&server, Duration::from_secs(5)).await;

    let mut handles = Vec::new();
    for _ in 0_i32..3_i32 {
        let url = url.clone();
        handles.push(tokio::spawn(async move {
            let c = connect_http_client(&url).await;
            let resp: RecallResponse = call_tool(&c, "recall", json!({"query": "Rust"})).await;
            assert!(resp.search_mode == SearchMode::Semantic || resp.search_mode == SearchMode::Hybrid);
            assert!(resp.count >= 1);
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    ct.cancel();
    server.shutdown().await;
}
