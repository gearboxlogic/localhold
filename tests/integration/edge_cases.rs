use std::{sync::Arc, time::Duration};

use chrono::{TimeDelta, Utc};
use localhold::{
    embedding::NoopEmbedding,
    server::{
        LocalHoldServer,
        params::{AdminListResponse, EvictExpiredResponse, ReadResponse, RecallResponse, ReembedResponse, RememberManyResponse, RememberResponse},
    },
    store::{MemoryWriter as _, SqliteStore},
    types::{AccessPolicy, Memory, Provenance, SearchMode},
};
use serde_json::json;

use super::helpers::{
    FailingEmbedding, ToggleableEmbedding, assert_invalid_params_contains, await_embeddings, call_tool, call_tool_params, setup_noop_server, setup_server_with,
    setup_server_with_store,
};

struct InventorySeed<'a> {
    content: &'a str,
    tags: &'a [&'a str],
    source_agent: &'a str,
    source_conversation: Option<&'a str>,
    origin_conversation: Option<&'a str>,
}

async fn seed_inventory_memory(server: &LocalHoldServer, seed: InventorySeed<'_>) -> localhold::types::MemoryId {
    let provenance = Provenance::new_for_test(
        Some(seed.source_agent.to_owned()),
        seed.source_conversation.map(ToOwned::to_owned),
        seed.origin_conversation.map(ToOwned::to_owned),
    );
    let memory = Memory::new_for_test(
        seed.content.to_owned(),
        seed.tags.iter().map(ToString::to_string).collect(),
        provenance,
        AccessPolicy::Public,
    );
    server.store().store(&memory, None).await.unwrap()
}

#[tokio::test]
async fn minimal_fields_defaults_apply() {
    let client = setup_noop_server().await;

    let remembered: RememberResponse = call_tool(&client, "remember", json!({"content": "minimal"})).await;

    let read: ReadResponse = call_tool(&client, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.memory.content, "minimal");
    assert!(read.memory.tags.is_empty());
    assert!(read.memory.expires_at.is_none());
    assert!(remembered.unresolved_scope);
    assert_eq!(remembered.scope, "inbox/unresolved");
}

#[tokio::test]
async fn optional_agent_facing_fields_populated() {
    let client = setup_noop_server().await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "fully populated",
            "summary": "fully populated summary",
            "tags": ["a", "b"],
            "agent_label": "agent-1",
            "scope": "conv-123",
            "access_policy": {"type": "restricted", "allowed": ["anonymous"]},
            "memory_type": "procedural",
            "importance": 0.8_f64,
            "confidence": 0.7_f64,
            "entities": [{"name": "recall", "type": "system"}]
        }),
    )
    .await;

    let read: ReadResponse = call_tool(&client, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.memory.content, "fully populated");
    assert_eq!(read.memory.tags, vec!["a", "b"]);
    assert_eq!(read.summary.as_deref(), Some("fully populated summary"));
    assert_eq!(read.agent_label.as_deref(), Some("agent-1"));
    assert_eq!(read.scope.as_deref(), Some("conv-123"));
    assert!(read.memory.expires_at.is_none(), "v2 core writes do not expose TTL");
}

#[tokio::test]
async fn sql_injection_in_content_and_recall() {
    let client = setup_noop_server().await;

    let injection: RememberResponse = call_tool(&client, "remember", json!({"content": "'; DROP TABLE memories; --"})).await;
    let _safe: RememberResponse = call_tool(&client, "remember", json!({"content": "safe content"})).await;

    let resp: RecallResponse = call_tool(&client, "recall", json!({"query": "'; DROP TABLE memories; --"})).await;
    assert_eq!(resp.count, 1);
    assert_eq!(resp.results[0].id, injection.id);

    let listed: AdminListResponse = call_tool(&client, "admin_list", json!({})).await;
    assert_eq!(listed.count, 2);
}

#[tokio::test]
async fn like_wildcards_escaped_in_protocol() {
    let client = setup_noop_server().await;

    let percent: RememberResponse = call_tool(&client, "remember", json!({"content": "100% complete"})).await;
    let _thousand: RememberResponse = call_tool(&client, "remember", json!({"content": "1000 items"})).await;

    let resp: RecallResponse = call_tool(&client, "recall", json!({"query": "100%"})).await;
    assert_eq!(resp.count, 1);
    assert_eq!(resp.results[0].id, percent.id);
}

#[tokio::test]
async fn admin_list_text_search_filters_results() {
    let client = setup_noop_server().await;

    let rust: RememberResponse = call_tool(&client, "remember", json!({"content": "rust programming language"})).await;
    let _python: RememberResponse = call_tool(&client, "remember", json!({"content": "python data science"})).await;
    let _cooking: RememberResponse = call_tool(&client, "remember", json!({"content": "cooking pasta recipe"})).await;

    let resp: AdminListResponse = call_tool(&client, "admin_list", json!({"text_search": "rust"})).await;
    assert_eq!(resp.count, 1);
    assert_eq!(resp.memories[0].id, rust.id);
}

#[tokio::test]
async fn admin_cleanup_expired_deletes_only_expired_memories() {
    let (client, server) = setup_server_with(Arc::new(NoopEmbedding::new())).await;
    let provenance = Provenance::new_for_test(Some("agent".into()), Some("cleanup-scope".into()), Some("cleanup-scope".into()));

    let mut expired = Memory::new_for_test("short-lived".into(), vec![], provenance.clone(), AccessPolicy::Public);
    expired.expires_at = Some(Utc::now() - TimeDelta::seconds(60));
    let _expired_id = server.store().store(&expired, None).await.unwrap();

    let durable = Memory::new_for_test("durable".into(), vec![], provenance, AccessPolicy::Public);
    let durable_id = server.store().store(&durable, None).await.unwrap();

    let cleaned: EvictExpiredResponse = call_tool(&client, "admin_cleanup_expired", json!({})).await;
    assert_eq!(cleaned.deleted, 1);

    let listed: AdminListResponse = call_tool(&client, "admin_list", json!({})).await;
    assert_eq!(listed.count, 1);
    assert_eq!(listed.memories[0].id, durable_id);
    assert_eq!(listed.memories[0].summary_or_excerpt, "durable");

    let cleaned_again: EvictExpiredResponse = call_tool(&client, "admin_cleanup_expired", json!({})).await;
    assert_eq!(cleaned_again.deleted, 0);
}

#[tokio::test]
async fn admin_list_hides_superseded_memories_unless_requested() {
    let (client, server) = setup_server_with(Arc::new(NoopEmbedding::new())).await;
    let provenance = Provenance::new_for_test(Some("agent".into()), Some("supersede-list".into()), Some("supersede-list".into()));

    let old = Memory::new_for_test("old version".into(), vec![], provenance.clone(), AccessPolicy::Public);
    let old_id = server.store().store(&old, None).await.unwrap();
    let new = Memory::new_for_test("new version".into(), vec![], provenance, AccessPolicy::Public);
    let new_id = server.store().store_with_supersession(&new, None, &old_id).await.unwrap();

    let old_read: ReadResponse = call_tool(&client, "read", json!({"id": old_id})).await;
    assert_eq!(old_read.memory.superseded_by, Some(new_id));

    let default_list: AdminListResponse = call_tool(&client, "admin_list", json!({"scope": "supersede-list"})).await;
    assert_eq!(default_list.count, 1);
    assert_eq!(default_list.memories[0].id, new_id);
    assert_eq!(default_list.memories[0].summary_or_excerpt, "new version");

    let with_superseded: AdminListResponse = call_tool(&client, "admin_list", json!({"scope": "supersede-list", "include_superseded": true})).await;
    assert_eq!(with_superseded.count, 2);
    assert!(with_superseded.memories.iter().any(|memory| memory.id == old_id && memory.superseded));
    assert!(with_superseded.memories.iter().any(|memory| memory.id == new_id && !memory.superseded));
}

#[tokio::test]
async fn recall_respects_default_limit_of_10() {
    let client = setup_noop_server().await;

    for i in 0_i32..15_i32 {
        let _resp: RememberResponse = call_tool(&client, "remember", json!({"content": format!("search-item-{i}")})).await;
    }

    let resp: RecallResponse = call_tool(&client, "recall", json!({"query": "search-item"})).await;
    assert!(resp.count <= 10, "recall should default to limit 10, got {}", resp.count);
}

#[tokio::test]
async fn admin_list_respects_default_limit_of_20() {
    let client = setup_noop_server().await;

    for i in 0_i32..25_i32 {
        let _resp: RememberResponse = call_tool(&client, "remember", json!({"content": format!("list-item-{i}")})).await;
    }

    let resp: AdminListResponse = call_tool(&client, "admin_list", json!({})).await;
    assert!(resp.count <= 20, "admin_list should default to limit 20, got {}", resp.count);
}

#[tokio::test]
async fn admin_list_scope_expansion_matches_ancestor_scopes() {
    let client = setup_noop_server().await;

    let project: RememberResponse = call_tool(&client, "remember", json!({"content": "project-level config", "scope": "org/project"})).await;
    let conversation: RememberResponse = call_tool(&client, "remember", json!({"content": "conversation-level note", "scope": "org/project/conv-123"})).await;
    let flat: RememberResponse = call_tool(&client, "remember", json!({"content": "flat scope memory", "scope": "global"})).await;

    let expanded: AdminListResponse = call_tool(
        &client,
        "admin_list",
        json!({
            "scope": "org/project/conv-123",
            "expand_scopes": true
        }),
    )
    .await;
    assert_eq!(expanded.count, 2, "expanded scope should include ancestor and exact scope memories");
    assert!(expanded.memories.iter().any(|memory| memory.id == project.id));
    assert!(expanded.memories.iter().any(|memory| memory.id == conversation.id));

    let exact: AdminListResponse = call_tool(
        &client,
        "admin_list",
        json!({
            "scope": "org/project/conv-123",
            "expand_scopes": false
        }),
    )
    .await;
    assert_eq!(exact.count, 1, "disabled scope expansion should match exact scope only");
    assert_eq!(exact.memories[0].id, conversation.id);

    let single: AdminListResponse = call_tool(
        &client,
        "admin_list",
        json!({
            "scope": "global",
            "expand_scopes": true
        }),
    )
    .await;
    assert_eq!(single.count, 1);
    assert_eq!(single.memories[0].id, flat.id);

    let empty: AdminListResponse = call_tool(
        &client,
        "admin_list",
        json!({
            "scopes": [],
            "expand_scopes": true
        }),
    )
    .await;
    assert_eq!(empty.count, 3, "empty scopes is treated like an omitted optional filter in v2");
}

#[tokio::test]
async fn admin_list_filters_by_provenance_scope_tags_and_origin() {
    let (client, server) = setup_server_with(Arc::new(NoopEmbedding::new())).await;

    let alpha = seed_inventory_memory(&server, InventorySeed {
        content: "alpha",
        tags: &["a"],
        source_agent: "bot1",
        source_conversation: Some("conv-1"),
        origin_conversation: Some("origin-a"),
    })
    .await;
    let beta = seed_inventory_memory(&server, InventorySeed {
        content: "beta",
        tags: &["a", "b"],
        source_agent: "bot2",
        source_conversation: Some("conv-2"),
        origin_conversation: Some("origin-b"),
    })
    .await;
    let gamma = seed_inventory_memory(&server, InventorySeed {
        content: "gamma",
        tags: &["b"],
        source_agent: "bot1",
        source_conversation: None,
        origin_conversation: None,
    })
    .await;
    let project = seed_inventory_memory(&server, InventorySeed {
        content: "project note",
        tags: &[],
        source_agent: "bot",
        source_conversation: Some("project-1"),
        origin_conversation: Some("conv-a"),
    })
    .await;

    let combined: AdminListResponse = call_tool(&client, "admin_list", json!({"tags": ["a"], "agent_label": "bot1"})).await;
    assert_eq!(combined.count, 1);
    assert_eq!(combined.memories[0].id, alpha);

    let by_conversation: AdminListResponse = call_tool(&client, "admin_list", json!({"scope": "conv-1"})).await;
    assert_eq!(by_conversation.count, 1);
    assert_eq!(by_conversation.memories[0].id, alpha);

    let by_scope: AdminListResponse = call_tool(&client, "admin_list", json!({"scopes": ["conv-2", "conv-3"]})).await;
    assert_eq!(by_scope.count, 1);
    assert_eq!(by_scope.memories[0].id, beta);

    let by_origin: AdminListResponse = call_tool(&client, "admin_list", json!({"origin_scope": "origin-b"})).await;
    assert_eq!(by_origin.count, 1);
    assert_eq!(by_origin.memories[0].id, beta);

    let trimmed_origin: AdminListResponse = call_tool(&client, "admin_list", json!({"scope": "  project-1  ", "origin_scope": "  conv-a  "})).await;
    assert_eq!(trimmed_origin.count, 1);
    assert_eq!(trimmed_origin.memories[0].id, project);

    let trimmed_scope: AdminListResponse = call_tool(&client, "admin_list", json!({"scopes": ["  project-1  "]})).await;
    assert_eq!(trimmed_scope.count, 1);
    assert_eq!(trimmed_scope.memories[0].id, project);

    let trimmed_agent: AdminListResponse = call_tool(&client, "admin_list", json!({"agent_label": "  bot1  "})).await;
    assert_eq!(trimmed_agent.count, 2);
    assert!(trimmed_agent.memories.iter().any(|memory| memory.id == alpha));
    assert!(trimmed_agent.memories.iter().any(|memory| memory.id == gamma));
}

#[tokio::test]
async fn admin_list_rejects_blank_filter_fields() {
    let client = setup_noop_server().await;

    for (args, expected_fragment) in [
        (json!({"agent_label": "   "}), "agent_label"),
        (json!({"scope": " \t "}), "scope"),
        (json!({"origin_scope": " \n "}), "origin_scope"),
        (json!({"scopes": ["project", "  "]}), "scopes"),
        (json!({"tags": ["decision", ""]}), "tags"),
    ] {
        assert_invalid_params_contains(&client, "admin_list", args, expected_fragment).await;
    }
}

// ---------------------------------------------------------------------------
// Graceful degradation tests (FailingEmbedding / ToggleableEmbedding)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn recall_falls_back_to_text_on_embedding_failure() {
    let (client, server) = setup_server_with(Arc::new(FailingEmbedding::provider())).await;

    let stored: RememberResponse = call_tool(&client, "remember", json!({"content": "findable needle in haystack"})).await;
    await_embeddings(&server, Duration::from_secs(5)).await;

    let resp: RecallResponse = call_tool(&client, "recall", json!({"query": "findable needle"})).await;
    assert_eq!(resp.search_mode, SearchMode::Keyword, "expected keyword fallback when embedding fails");
    assert_eq!(resp.count, 1, "text search should find the stored memory");
    assert_eq!(resp.results[0].id, stored.id);
}

#[tokio::test]
async fn explicit_hybrid_recall_does_not_silently_degrade() {
    let client = setup_noop_server().await;

    let _stored: RememberResponse = call_tool(&client, "remember", json!({"content": "findable hybrid candidate"})).await;

    let err = client
        .call_tool(call_tool_params("recall", json!({"query": "findable", "search_mode": "hybrid"})))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("search unavailable"), "expected explicit hybrid request to fail, got: {err}");
}

#[tokio::test]
async fn explicit_semantic_recall_does_not_silently_degrade() {
    let client = setup_noop_server().await;

    let _stored: RememberResponse = call_tool(&client, "remember", json!({"content": "findable semantic candidate"})).await;

    let err = client
        .call_tool(call_tool_params("recall", json!({"query": "findable", "search_mode": "semantic"})))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("search unavailable"), "expected explicit semantic request to fail, got: {err}");
}

#[tokio::test]
async fn explicit_keyword_recall_does_not_silently_degrade_when_fts_is_unavailable() {
    let store = SqliteStore::in_memory().unwrap();
    store.set_fts_available_for_test(false);
    let (client, _server) = setup_server_with_store(store, Arc::new(NoopEmbedding::new())).await;

    let err = client
        .call_tool(call_tool_params("recall", json!({"query": "findable", "search_mode": "keyword"})))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("search unavailable"), "expected explicit keyword request to fail, got: {err}");
}

#[tokio::test]
async fn remember_succeeds_when_embedding_fails() {
    let (client, server) = setup_server_with(Arc::new(FailingEmbedding::unavailable())).await;

    let stored: RememberResponse = call_tool(&client, "remember", json!({"content": "persists without embedding"})).await;

    await_embeddings(&server, Duration::from_secs(5)).await;

    let read: ReadResponse = call_tool(&client, "read", json!({"id": stored.id})).await;
    assert_eq!(read.memory.content, "persists without embedding");
    assert!(!read.memory.has_embedding, "embedding should not have been stored");
}

#[tokio::test]
async fn embedding_toggle_degrades_gracefully() {
    let (toggleable, flag) = ToggleableEmbedding::new(true);
    let (client, server) = setup_server_with(Arc::new(toggleable)).await;

    let first: RememberResponse = call_tool(&client, "remember", json!({"content": "first embedded memory"})).await;
    await_embeddings(&server, Duration::from_secs(5)).await;

    let read: ReadResponse = call_tool(&client, "read", json!({"id": first.id})).await;
    assert!(read.memory.has_embedding, "first memory should be embedded");

    flag.store(false, std::sync::atomic::Ordering::Relaxed);

    let second: RememberResponse = call_tool(&client, "remember", json!({"content": "second unembedded memory"})).await;
    await_embeddings(&server, Duration::from_secs(5)).await;

    let read: ReadResponse = call_tool(&client, "read", json!({"id": second.id})).await;
    assert!(!read.memory.has_embedding, "second memory should lack embedding");

    let resp: RecallResponse = call_tool(&client, "recall", json!({"query": "second unembedded"})).await;
    assert_eq!(resp.search_mode, SearchMode::Keyword, "recall should fall back to keyword when embedding disabled");
    assert_eq!(resp.count, 1);

    flag.store(true, std::sync::atomic::Ordering::Relaxed);

    let resp: ReembedResponse = call_tool(&client, "admin_reembed", json!({})).await;
    assert_eq!(resp.queued, 1, "only the unembedded memory should be queued");

    await_embeddings(&server, Duration::from_secs(5)).await;

    let listed: AdminListResponse = call_tool(&client, "admin_list", json!({"has_embedding": true})).await;
    assert_eq!(listed.count, 2, "both memories should now have embeddings");
}

#[tokio::test]
async fn remember_many_succeeds_when_embedding_fails() {
    let (client, server) = setup_server_with(Arc::new(FailingEmbedding::provider())).await;

    let resp: RememberManyResponse = call_tool(
        &client,
        "remember_many",
        json!({
            "memories": [
                {"content": "batch item alpha"},
                {"content": "batch item beta"},
                {"content": "batch item gamma"}
            ]
        }),
    )
    .await;

    assert_eq!(resp.memories.len(), 3, "all three memories should be stored");

    await_embeddings(&server, Duration::from_secs(5)).await;

    let listed: AdminListResponse = call_tool(&client, "admin_list", json!({})).await;
    assert_eq!(listed.count, 3, "all memories should be listed");

    let unembedded: AdminListResponse = call_tool(&client, "admin_list", json!({"has_embedding": false})).await;
    assert_eq!(unembedded.count, 3, "none should have embeddings");
}
