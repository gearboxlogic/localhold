use std::{sync::Arc, time::Duration};

use localhold::{
    clock::MockClock,
    config::LimitsConfig,
    embedding::NoopEmbedding,
    server::params::{AdminListResponse, CountResponse, OperationStatus, ReadResponse, RememberManyResponse, RememberResponse},
    store::MemoryWriter as _,
    types::{AccessPolicy, Memory, MemoryType, Provenance},
};
use serde_json::json;

use super::helpers::{
    assert_invalid_params_contains, await_embeddings, call_tool, call_tool_error, setup_embedding_server, setup_noop_server, setup_noop_server_with_clock,
    setup_noop_server_with_limits, setup_server_with,
};

// ===========================================================================
// remember_many tests (stdio-only; common cases in transport_matrix)
// ===========================================================================

#[tokio::test]
async fn remember_many_preserves_agent_facing_metadata() {
    let client = setup_noop_server().await;

    let resp: RememberManyResponse = call_tool(
        &client,
        "remember_many",
        json!({
            "memories": [{
                "content": "metadata test",
                "summary": "metadata summary",
                "tags": ["tag-a", "tag-b"],
                "agent_label": "bot-1",
                "scope": "conv-42",
                "access_policy": {"type": "restricted", "allowed": ["anonymous"]}
            }]
        }),
    )
    .await;
    assert_eq!(resp.operation.status, OperationStatus::Applied);
    assert_eq!(resp.operation.changed, 1);
    assert_eq!(resp.memories.len(), 1);
    assert_eq!(resp.memories[0].scope, "conv-42");

    let read: ReadResponse = call_tool(&client, "read", json!({"id": resp.memories[0].id})).await;
    assert_eq!(read.memory.content, "metadata test");
    assert_eq!(read.memory.tags, vec!["tag-a", "tag-b"]);
    assert_eq!(read.summary.as_deref(), Some("metadata summary"));
    assert_eq!(read.agent_label.as_deref(), Some("bot-1"));
    assert_eq!(read.scope.as_deref(), Some("conv-42"));
}

#[tokio::test]
async fn remember_many_defaults_to_unresolved_inbox_without_scope() {
    let client = setup_noop_server().await;

    let resp: RememberManyResponse = call_tool(
        &client,
        "remember_many",
        json!({
            "memories": [{
                "content": "origin default test"
            }]
        }),
    )
    .await;

    assert_eq!(resp.memories.len(), 1);
    assert!(resp.memories[0].unresolved_scope);
    assert_eq!(resp.memories[0].scope, "inbox/unresolved");
    assert!(
        resp.operation.warnings.iter().any(|warning| warning.code == "missing_scope"),
        "unscoped writes should warn agents to classify later"
    );
}

#[tokio::test]
async fn remember_many_rejects_oversized_batch() {
    let client = setup_noop_server().await;

    let items: Vec<_> = (0_i32..101_i32).map(|i| json!({"content": format!("item-{i}")})).collect();

    assert_invalid_params_contains(&client, "remember_many", json!({"memories": items}), "exceeds maximum").await;
}

#[tokio::test]
async fn remember_many_rejects_oversized_batch_before_item_validation() {
    let mut limits = LimitsConfig::default();
    limits.max_batch_size = 1;
    let (client, _server) = setup_noop_server_with_limits(limits).await;

    let err = call_tool_error(
        &client,
        "remember_many",
        json!({
            "memories": [
                {"content": "valid but oversized", "context_hints": [""]},
                {"content": "second item"}
            ]
        }),
    )
    .await;

    assert!(err.contains("exceeds maximum batch size of 1"), "expected batch cap error, got: {err}");
    assert!(!err.contains("context_hints"), "oversized batches should fail before per-item validation: {err}");
}

#[tokio::test]
async fn remember_many_rejects_empty_batch() {
    let client = setup_noop_server().await;

    assert_invalid_params_contains(&client, "remember_many", json!({"memories": []}), "cannot be empty").await;
}

#[tokio::test]
async fn remember_many_rejects_blank_content() {
    let client = setup_noop_server().await;

    assert_invalid_params_contains(
        &client,
        "remember_many",
        json!({
            "memories": [
                {"content": "valid"},
                {"content": "   "}
            ]
        }),
        "content",
    )
    .await;
}

#[tokio::test]
async fn remember_many_rejects_blank_context_hint() {
    let client = setup_noop_server().await;

    assert_invalid_params_contains(
        &client,
        "remember_many",
        json!({
            "memories": [{
                "content": "valid content",
                "context_hints": [" \t "]
            }]
        }),
        "context_hints",
    )
    .await;
}

#[tokio::test]
async fn remember_many_rejects_invalid_access_policy() {
    let client = setup_noop_server().await;

    assert_invalid_params_contains(
        &client,
        "remember_many",
        json!({
            "memories": [{
                "content": "valid content",
                "access_policy": {"bad": "format"}
            }]
        }),
        "failed to deserialize",
    )
    .await;
}

#[tokio::test]
async fn remember_many_background_embeddings_are_visible_in_admin_inventory() {
    let (client, server) = setup_embedding_server().await;

    let resp: RememberManyResponse = call_tool(
        &client,
        "remember_many",
        json!({
            "memories": [
                {"content": "embed batch 1"},
                {"content": "embed batch 2"},
                {"content": "embed batch 3"}
            ]
        }),
    )
    .await;
    assert_eq!(resp.memories.len(), 3);

    await_embeddings(&server, Duration::from_secs(5)).await;

    let listed: AdminListResponse = call_tool(&client, "admin_list", json!({"has_embedding": true})).await;
    assert_eq!(listed.count, 3);

    server.shutdown().await;
}

// ===========================================================================
// admin_count tests (stdio-only; common cases in transport_matrix)
// ===========================================================================

#[tokio::test]
async fn admin_count_tag_breakdown() {
    let client = setup_noop_server().await;

    for _ in 0_i32..3_i32 {
        let _stored: RememberResponse = call_tool(&client, "remember", json!({"content": "tagged", "tags": ["frequent"]})).await;
    }
    let _rare: RememberResponse = call_tool(&client, "remember", json!({"content": "tagged", "tags": ["rare"]})).await;

    let count: CountResponse = call_tool(&client, "admin_count", json!({})).await;
    assert_eq!(count.total, 4);
    assert!(!count.by_tag.is_empty());
    assert_eq!(count.by_tag[0].tag, "frequent");
    assert_eq!(count.by_tag[0].count, 3);
}

#[tokio::test]
async fn admin_count_scope_filter_and_breakdown() {
    let client = setup_noop_server().await;

    let _visible: RememberResponse = call_tool(&client, "remember", json!({"content": "visible", "scope": "visible-scope"})).await;
    let _hidden: RememberResponse = call_tool(&client, "remember", json!({"content": "hidden", "scope": "hidden-scope"})).await;

    let filtered: CountResponse = call_tool(
        &client,
        "admin_count",
        json!({
            "scope": "visible-scope"
        }),
    )
    .await;
    assert_eq!(filtered.total, 1);
    assert_eq!(filtered.scope_count, 1, "scope_count should describe the filtered subset");
    assert_eq!(filtered.by_scope.iter().map(|entry| (entry.scope.as_str(), entry.count)).collect::<Vec<_>>(), vec![(
        "visible-scope",
        1
    )]);
    assert_eq!(filtered.superseded_count, 0, "new writes should not be superseded");
}

#[tokio::test]
async fn admin_count_reports_storage_scope_and_memory_type_breakdowns() {
    let client = setup_noop_server().await;

    for (content, memory_type, scope) in [
        ("semantic memory 1", "semantic", "scope-a"),
        ("episodic memory 1", "episodic", "scope-b"),
        ("procedural memory 1", "procedural", "scope-a"),
        ("semantic memory 2", "semantic", "scope-b"),
    ] {
        let _stored: RememberResponse = call_tool(
            &client,
            "remember",
            json!({
                "content": content,
                "memory_type": memory_type,
                "scope": scope
            }),
        )
        .await;
    }

    let count: CountResponse = call_tool(&client, "admin_count", json!({})).await;
    assert_eq!(count.total, 4);
    assert!(count.storage_bytes.is_some_and(|bytes| bytes > 0), "storage_bytes should be positive");
    assert!(count.oldest_memory.is_some(), "oldest_memory should be populated");
    assert!(count.newest_memory.is_some(), "newest_memory should be populated");
    assert_eq!(
        count.by_scope.iter().map(|entry| (entry.scope.as_str(), entry.count)).collect::<Vec<_>>(),
        vec![("scope-a", 2), ("scope-b", 2)],
        "scope breakdown should be deterministic and count exact assignments"
    );
    assert_eq!(count.scope_count, 2, "scope_count should count distinct scopes");
    assert_eq!(count.superseded_count, 0, "new writes should not be superseded");

    let semantic = count.by_memory_type.iter().find(|entry| entry.memory_type == MemoryType::Semantic);
    assert_eq!(semantic.map(|entry| entry.count), Some(2));
    let episodic = count.by_memory_type.iter().find(|entry| entry.memory_type == MemoryType::Episodic);
    assert_eq!(episodic.map(|entry| entry.count), Some(1));
    let procedural = count.by_memory_type.iter().find(|entry| entry.memory_type == MemoryType::Procedural);
    assert_eq!(procedural.map(|entry| entry.count), Some(1));
}

#[tokio::test]
async fn admin_count_reports_superseded_rows_when_requested() {
    let (client, server) = setup_server_with(Arc::new(NoopEmbedding::new())).await;
    let provenance = Provenance::new_for_test(Some("agent".into()), Some("supersede-scope".into()), Some("supersede-scope".into()));

    let original = Memory::new_for_test("original memory".into(), vec![], provenance.clone(), AccessPolicy::Public);
    let original_id = server.store().store(&original, None).await.unwrap();
    let replacement = Memory::new_for_test("updated memory".into(), vec![], provenance, AccessPolicy::Public);
    let _replacement_id = server.store().store_with_supersession(&replacement, None, &original_id).await.unwrap();

    let default_count: CountResponse = call_tool(&client, "admin_count", json!({})).await;
    assert_eq!(default_count.total, 1, "default count should hide superseded rows");
    assert_eq!(default_count.superseded_count, 0, "default count describes the visible subset");

    let with_superseded: CountResponse = call_tool(&client, "admin_count", json!({"include_superseded": true})).await;
    assert_eq!(with_superseded.total, 2);
    assert_eq!(with_superseded.superseded_count, 1);
}

#[tokio::test]
async fn admin_count_with_no_memories() {
    let client = setup_noop_server().await;

    let count: CountResponse = call_tool(&client, "admin_count", json!({})).await;
    assert_eq!(count.total, 0);
    assert_eq!(count.with_embedding, 0);
    assert_eq!(count.without_embedding, 0);
    assert_eq!(count.expired, 0);
    assert!(count.by_tag.is_empty());
    assert!(count.by_agent_label.is_empty());
}

#[tokio::test]
async fn admin_count_rejects_blank_filter_fields() {
    let client = setup_noop_server().await;

    assert_invalid_params_contains(&client, "admin_count", json!({"agent_label": "   "}), "agent_label").await;

    assert_invalid_params_contains(&client, "admin_count", json!({"scope": " \t "}), "scope").await;
}

#[tokio::test]
async fn admin_count_expired_memories_reports_global_expired_count() {
    let mock = Arc::new(MockClock::new());
    let (client, _server) = setup_noop_server_with_clock(Arc::clone(&mock)).await;

    let _ephemeral: RememberResponse = call_tool(&client, "remember", json!({"content": "ephemeral"})).await;
    let _durable: RememberResponse = call_tool(&client, "remember", json!({"content": "durable"})).await;

    mock.advance(chrono::TimeDelta::seconds(2));

    let stats: CountResponse = call_tool(&client, "admin_count", json!({})).await;
    assert_eq!(stats.total, 2);
    assert_eq!(stats.expired, 0, "core writes do not expose TTL");
}
