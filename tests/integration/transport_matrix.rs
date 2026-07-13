//! Transport-agnostic tests that run against both stdio and HTTP.
//!
//! Each [`transport_test!`] invocation generates two `#[tokio::test]`
//! functions: `stdio_<name>` and `http_<name>`.

use std::time::Duration;

use localhold::{
    server::params::{
        AdminListResponse, CountResponse, DeleteResponse, OperationStatus, ReadResponse, ReassignScopeResponse, RecallResponse, RememberManyResponse, RememberResponse,
        UpdateResponse,
    },
    types::SearchMode,
};
use serde_json::json;

use super::helpers::{assert_invalid_params_contains, await_embeddings, call_tool, call_tool_error, transport_test};

transport_test!(noop, core_lifecycle, |h| async move {
    let client = h.client();

    let remembered: RememberResponse = call_tool(
        client,
        "remember",
        json!({
            "content": "transport lifecycle memory",
            "summary": "transport lifecycle",
            "scope": "matrix/lifecycle",
            "tags": ["matrix", "crud"],
            "agent_label": "transport-test"
        }),
    )
    .await;
    assert_eq!(remembered.operation.status, OperationStatus::Applied);
    assert_eq!(remembered.scope, "matrix/lifecycle");

    let listed: AdminListResponse = call_tool(client, "admin_list", json!({"tags": ["crud"], "scope": "matrix/lifecycle"})).await;
    assert_eq!(listed.count, 1);
    assert_eq!(listed.memories[0].summary_or_excerpt, "transport lifecycle");

    let recalled: RecallResponse = call_tool(client, "recall", json!({"query": "transport lifecycle", "scope": "matrix/lifecycle"})).await;
    assert_eq!(recalled.count, 1);
    assert_eq!(recalled.results[0].id, remembered.id);

    let revised: UpdateResponse = call_tool(
        client,
        "revise",
        json!({
            "id": remembered.id,
            "content": "transport lifecycle memory revised",
            "summary": "transport lifecycle revised"
        }),
    )
    .await;
    assert!(revised.updated);

    let read: ReadResponse = call_tool(client, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.memory.content, "transport lifecycle memory revised");
    assert_eq!(read.summary.as_deref(), Some("transport lifecycle revised"));
    assert!(read.activity_recorded);

    let deleted: DeleteResponse = call_tool(client, "forget", json!({"id": remembered.id})).await;
    assert!(deleted.deleted);

    let err = call_tool_error(client, "read", json!({"id": remembered.id})).await;
    assert!(err.contains("not found"));

    h.shutdown().await;
});

transport_test!(noop, remember_many_and_admin_count, |h| async move {
    let client = h.client();

    let remembered: RememberManyResponse = call_tool(
        client,
        "remember_many",
        json!({
            "memories": [
                {"content": "matrix batch one", "scope": "matrix/batch", "tags": ["batch", "one"]},
                {"content": "matrix batch two", "scope": "matrix/batch", "tags": ["batch", "two"]},
                {"content": "matrix other", "scope": "matrix/other", "tags": ["other"]}
            ]
        }),
    )
    .await;
    assert_eq!(remembered.operation.status, OperationStatus::Applied);
    assert_eq!(remembered.memories.len(), 3);

    let listed: AdminListResponse = call_tool(client, "admin_list", json!({"scope": "matrix/batch", "limit": 10_i32})).await;
    assert_eq!(listed.count, 2);
    assert!(listed.memories.iter().all(|memory| memory.scope == "matrix/batch"));

    let count: CountResponse = call_tool(client, "admin_count", json!({"scope": "matrix/batch"})).await;
    assert_eq!(count.total, 2);
    assert!(count.by_tag.iter().any(|tag| tag.tag == "batch" && tag.count == 2));

    h.shutdown().await;
});

transport_test!(noop, scope_registry_context_hints, |h| async move {
    let client = h.client();

    let _registered: serde_json::Value = call_tool(
        client,
        "admin_scope_register",
        json!({
            "scope_key": "matrix/scope",
            "display_name": "Matrix Scope",
            "description": "Transport matrix scope",
            "aliases": ["matrix-alias"],
            "matchers": ["/repo/matrix"]
        }),
    )
    .await;

    let remembered: RememberResponse = call_tool(
        client,
        "remember",
        json!({
            "content": "context hint scoped memory",
            "context_hints": ["/repo/matrix/src/lib.rs"]
        }),
    )
    .await;
    assert_eq!(remembered.scope, "matrix/scope");
    assert!(!remembered.unresolved_scope);

    let recalled: RecallResponse = call_tool(
        client,
        "recall",
        json!({
            "query": "context hint scoped",
            "scope": "matrix-alias"
        }),
    )
    .await;
    assert_eq!(recalled.count, 1);
    assert_eq!(recalled.results[0].scope, "matrix/scope");

    h.shutdown().await;
});

transport_test!(noop, admin_reassign_scope_updates_read_metadata, |h| async move {
    let client = h.client();

    let remembered: RememberResponse = call_tool(client, "remember", json!({"content": "move this scope", "scope": "matrix/source"})).await;

    let moved: ReassignScopeResponse = call_tool(client, "admin_reassign_scope", json!({"from_scope": "matrix/source", "to_scope": "matrix/destination"})).await;
    assert_eq!(moved.reassigned, 1);

    let read: ReadResponse = call_tool(client, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.scope.as_deref(), Some("matrix/destination"));

    let listed: AdminListResponse = call_tool(client, "admin_list", json!({"scope": "matrix/destination"})).await;
    assert_eq!(listed.count, 1);
    assert_eq!(listed.memories[0].id, remembered.id);

    h.shutdown().await;
});

transport_test!(noop, validation_errors_are_transport_consistent, |h| async move {
    let client = h.client();

    assert_invalid_params_contains(client, "remember", json!({}), "content").await;
    assert_invalid_params_contains(client, "remember", json!({"content": "   "}), "blank").await;
    assert_invalid_params_contains(client, "remember", json!({"content": "x", "scope": "   "}), "scope").await;
    assert_invalid_params_contains(client, "recall", json!({}), "query").await;
    assert_invalid_params_contains(client, "recall", json!({"query": "   "}), "blank").await;
    assert_invalid_params_contains(client, "read", json!({}), "id").await;
    assert_invalid_params_contains(client, "read", json!({"id": "not-a-valid-ulid"}), "failed to deserialize").await;
    assert_invalid_params_contains(client, "revise", json!({}), "id").await;
    assert_invalid_params_contains(client, "forget", json!({}), "id").await;
    assert_invalid_params_contains(client, "remember_many", json!({}), "memories").await;

    h.shutdown().await;
});

transport_test!(noop, remember_many_validation_is_all_or_nothing, |h| async move {
    let client = h.client();

    assert_invalid_params_contains(
        client,
        "remember_many",
        json!({
            "memories": [
                {"content": "valid matrix batch item", "scope": "matrix/all-or-nothing"},
                {"content": "   ", "scope": "matrix/all-or-nothing"}
            ]
        }),
        "memories[1]",
    )
    .await;

    let listed: AdminListResponse = call_tool(client, "admin_list", json!({"scope": "matrix/all-or-nothing"})).await;
    assert_eq!(listed.count, 0);

    h.shutdown().await;
});

transport_test!(embedding, recall_uses_embeddings_when_available, |h| async move {
    let client = h.client();

    let rust: RememberResponse = call_tool(client, "remember", json!({"content": "Rust programming language systems", "scope": "matrix/embedding"})).await;
    let _python: RememberResponse = call_tool(client, "remember", json!({"content": "Python data science machine learning", "scope": "matrix/embedding"})).await;
    let _cooking: RememberResponse = call_tool(client, "remember", json!({"content": "cooking pasta carbonara recipe", "scope": "matrix/embedding"})).await;

    await_embeddings(h.server(), Duration::from_secs(5)).await;

    let read: ReadResponse = call_tool(client, "read", json!({"id": rust.id})).await;
    assert!(read.memory.has_embedding);

    let recalled: RecallResponse = call_tool(
        client,
        "recall",
        json!({
            "query": "Rust programming",
            "scope": "matrix/embedding",
            "search_mode": "semantic"
        }),
    )
    .await;
    assert_eq!(recalled.search_mode, SearchMode::Semantic);
    assert!(recalled.count >= 1);
    assert!(recalled.results.iter().any(|result| result.diagnostics.vector_distance.is_some()));

    h.shutdown().await;
});

transport_test!(embedding, revise_content_triggers_reembedding, |h| async move {
    let client = h.client();

    let remembered: RememberResponse = call_tool(client, "remember", json!({"content": "original embedding matrix", "scope": "matrix/reembed"})).await;

    await_embeddings(h.server(), Duration::from_secs(5)).await;
    let before: ReadResponse = call_tool(client, "read", json!({"id": remembered.id})).await;
    assert!(before.memory.has_embedding);

    let revised: UpdateResponse = call_tool(client, "revise", json!({"id": remembered.id, "content": "changed embedding matrix"})).await;
    assert!(revised.updated);

    await_embeddings(h.server(), Duration::from_secs(5)).await;
    let after: ReadResponse = call_tool(client, "read", json!({"id": remembered.id})).await;
    assert!(after.memory.has_embedding);
    assert_eq!(after.memory.content, "changed embedding matrix");

    h.shutdown().await;
});
