//! Protocol-level tests that verify tool schemas, server capabilities, and
//! behaviour unique to the stdio transport.  Tests that are semantically
//! identical across transports have been moved to `transport_matrix.rs`.

use std::{sync::Arc, time::Duration};

use chrono::Utc;
use localhold::{
    clock::MockClock,
    config::{AnonymousPolicy, LimitsConfig, SearchConfig},
    embedding::NoopEmbedding,
    engine::RecallEngine,
    reranker::{RerankerError, RerankerProvider, RerankerScore},
    server::{
        RecallServer,
        params::{
            AdminListResponse, AdminV2MigrateMetadataResponse, AdminV2MigrationReportResponse, BriefResponse, BulkDeleteResponse, BulkUpdateResponse, ConsolidateResponse,
            CountResponse, HandoffResponse, HistoryResponse, MatchAction, MatchQuality, OperationStatus, QualityWarning, QualityWarningSeverity, ReadManyResponse, ReadManyStatus,
            ReadResponse, ReassignScopeResponse, RecallResponse, RecommendedActionPriority, RecommendedActionTool, RememberManyResponse, RememberResponse, ScopeResolvedBy,
            ToolErrorCode, ToolErrorResponse, UpdateResponse,
        },
    },
    store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
    types::{AccessPolicy, AuditAction, Memory, MemoryId, Provenance},
};
use rmcp::{ServiceExt as _, service::RunningService};
use serde_json::json;

use super::{
    fault_injection::{chaos_store_fail_batch_and_store_call, chaos_store_fail_on_store_call},
    helpers::{
        DeterministicEmbedding, LegacySeed, assert_invalid_params_contains, await_embeddings, call_tool, call_tool_error, call_tool_params, setup_embedding_server,
        setup_noop_server, setup_noop_server_with_auth, setup_noop_server_with_auth_and_legacy_memories, setup_noop_server_with_clock, setup_noop_server_with_legacy_memories,
        setup_noop_server_with_limits, setup_server_with, setup_server_with_auth,
    },
};

struct LowScoreReranker;

impl RerankerProvider for LowScoreReranker {
    fn rerank<'a>(&'a self, _query: &'a str, documents: &'a [&'a str]) -> localhold::reranker::BoxFuture<'a, Result<Vec<RerankerScore>, RerankerError>> {
        let scores = documents.iter().enumerate().map(|(index, _document)| RerankerScore::new(index, 0.0_f64)).collect();
        Box::pin(async move { Ok(scores) })
    }

    fn health_check(&self) -> localhold::reranker::BoxFuture<'_, Result<(), RerankerError>> {
        Box::pin(async { Ok(()) })
    }
}

fn assert_warning_codes(warnings: &[QualityWarning], expected: &[&str]) {
    let codes = warnings.iter().map(|warning| warning.code.as_str()).collect::<Vec<_>>();
    for expected_code in expected {
        assert!(codes.contains(expected_code), "expected warning code {expected_code}; got {codes:?}");
    }
}

#[expect(clippy::panic, reason = "test helper: panic with diagnostic context on deserialization failure")]
fn parse_tool_error(text: &str) -> ToolErrorResponse {
    serde_json::from_str(text).unwrap_or_else(|err| panic!("expected structured tool error JSON: {err}; raw: {text}"))
}

async fn setup_low_rerank_server() -> (RunningService<rmcp::RoleClient, ()>, RecallServer) {
    let store = SqliteStore::in_memory().unwrap();
    let mut search_config = SearchConfig::default();
    search_config.reranker.blend_weight = 1.0_f64;
    let engine = RecallEngine::new(store, Arc::new(DeterministicEmbedding), LimitsConfig::default(), search_config).with_reranker(Arc::new(LowScoreReranker));
    let server = RecallServer::from_engine(engine);
    let server_ref = server.clone();
    let (server_transport, client_transport) = tokio::io::duplex(4096);

    let _server_task = tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });

    let client = ().serve(client_transport).await.unwrap();
    (client, server_ref)
}

async fn register_localhold_scope(client: &RunningService<rmcp::RoleClient, ()>) {
    let _scope: serde_json::Value = call_tool(
        client,
        "admin_scope_register",
        json!({
            "scope_key": "gearboxlogic/localhold",
            "display_name": "LocalHold"
        }),
    )
    .await;
}

async fn seed_reassign_memory(server: &RecallServer, content: &str, source_agent: &str, source_conversation: &str, origin_conversation: &str) -> MemoryId {
    let provenance = Provenance::new_for_test(Some(source_agent.to_owned()), Some(source_conversation.to_owned()), Some(origin_conversation.to_owned()));
    let memory = Memory::new_for_test(content.to_owned(), Vec::new(), provenance, AccessPolicy::Public);
    server.store().store(&memory, None).await.unwrap()
}

#[tokio::test]
async fn tool_list_returns_expected_tools() {
    let client = setup_noop_server().await;
    let tools = client.list_all_tools().await.unwrap();

    let mut names: Vec<&str> = tools.iter().map(|t| &*t.name).collect();
    names.sort_unstable();

    assert_eq!(names, vec![
        "admin_bulk_delete",
        "admin_bulk_update",
        "admin_cleanup_expired",
        "admin_consolidate",
        "admin_count",
        "admin_history",
        "admin_list",
        "admin_reassign_scope",
        "admin_reembed",
        "admin_scope_list",
        "admin_scope_register",
        "admin_v2_migrate_metadata",
        "admin_v2_migration_report",
        "brief",
        "forget",
        "handoff",
        "read",
        "read_many",
        "recall",
        "remember",
        "remember_many",
        "revise"
    ]);
    assert_eq!(names.len(), 22_usize, "expected 22 default-discovery tools");
    assert!(!names.contains(&"memory_store"), "legacy v1 tools should remain hidden from default discovery");
}

#[tokio::test]
async fn legacy_memory_tools_are_not_directly_callable() {
    let client = setup_noop_server().await;
    let result = client.call_tool(call_tool_params("memory_store", json!({}))).await;

    assert!(result.is_err(), "legacy memory_store should not be registered as an MCP tool");
}

#[tokio::test]
async fn tool_schemas_have_required_fields() {
    use std::collections::BTreeSet;

    let client = setup_noop_server().await;
    let tools = client.list_all_tools().await.unwrap();

    let find_required = |name: &str| -> BTreeSet<String> {
        #[expect(clippy::panic, reason = "test assertion with formatted message")]
        let tool = tools.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("tool {name} not found"));
        tool.input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect::<BTreeSet<_>>())
            .unwrap_or_default()
    };

    assert_eq!(find_required("remember"), BTreeSet::from(["content".to_owned()]));
    assert_eq!(find_required("remember_many"), BTreeSet::from(["memories".to_owned()]));
    assert_eq!(find_required("recall"), BTreeSet::from(["query".to_owned()]));
    assert_eq!(find_required("read"), BTreeSet::from(["id".to_owned()]));
    assert_eq!(find_required("read_many"), BTreeSet::from(["ids".to_owned()]));
    assert_eq!(find_required("revise"), BTreeSet::from(["id".to_owned()]));
    assert_eq!(find_required("forget"), BTreeSet::from(["id".to_owned()]));
    assert_eq!(find_required("handoff"), BTreeSet::from(["candidates".to_owned()]));
    assert!(find_required("admin_bulk_delete").is_empty());
    assert!(find_required("admin_bulk_update").is_empty());
    assert!(find_required("admin_cleanup_expired").is_empty());
    assert!(find_required("admin_consolidate").is_empty());
    assert!(find_required("admin_count").is_empty());
    assert!(find_required("admin_list").is_empty());
    assert_eq!(find_required("admin_history"), BTreeSet::from(["id".to_owned()]));
    assert_eq!(find_required("admin_reassign_scope"), BTreeSet::from(["from_scope".to_owned(), "to_scope".to_owned()]));
    assert!(find_required("admin_reembed").is_empty());
    assert_eq!(find_required("admin_scope_register"), BTreeSet::from(["display_name".to_owned(), "scope_key".to_owned()]));
    assert!(find_required("admin_scope_list").is_empty());
    assert!(find_required("admin_v2_migrate_metadata").is_empty());
    assert!(find_required("admin_v2_migration_report").is_empty());
}

#[tokio::test]
async fn tool_schemas_expose_v2_properties() {
    let client = setup_noop_server().await;
    let tools = client.list_all_tools().await.unwrap();

    let find_properties = |name: &str| -> Vec<String> {
        #[expect(clippy::panic, reason = "test assertion with formatted message")]
        let tool = tools.iter().find(|t| t.name == name).unwrap_or_else(|| panic!("tool {name} not found"));
        tool.input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .map(|obj| obj.keys().cloned().collect())
            .unwrap_or_default()
    };

    let remember_props = find_properties("remember");
    assert!(remember_props.contains(&"content".into()));
    assert!(remember_props.contains(&"summary".into()));
    assert!(remember_props.contains(&"scope".into()));
    assert!(remember_props.contains(&"context_hints".into()));
    assert!(remember_props.contains(&"agent_label".into()));

    let recall_props = find_properties("recall");
    assert!(recall_props.contains(&"query".into()));
    assert!(recall_props.contains(&"include_weak".into()));
    assert!(recall_props.contains(&"context_hints".into()));
    assert!(recall_props.contains(&"literal_terms".into()));
    assert!(recall_props.contains(&"query_context".into()));

    let remember_many_props = find_properties("remember_many");
    assert!(remember_many_props.contains(&"memories".into()));

    let read_many_props = find_properties("read_many");
    assert!(read_many_props.contains(&"ids".into()));

    let revise_props = find_properties("revise");
    assert!(revise_props.contains(&"summary".into()));
    assert!(revise_props.contains(&"agent_label".into()));
    assert!(revise_props.contains(&"scope".into()));
    assert!(revise_props.contains(&"context_hints".into()));

    let brief_props = find_properties("brief");
    assert!(brief_props.contains(&"context_hints".into()));

    let scope_props = find_properties("admin_scope_register");
    assert!(scope_props.contains(&"scope_key".into()));
    assert!(scope_props.contains(&"aliases".into()));
    assert!(scope_props.contains(&"matchers".into()));

    for admin_tool in ["admin_bulk_delete", "admin_bulk_update", "admin_count", "admin_list"] {
        let props = find_properties(admin_tool);
        assert!(props.contains(&"agent_label".to_owned()), "{admin_tool} should expose agent_label");
        assert!(props.contains(&"scope".to_owned()), "{admin_tool} should expose scope");
        assert!(props.contains(&"scopes".to_owned()), "{admin_tool} should expose scopes");
        assert!(props.contains(&"origin_scope".to_owned()), "{admin_tool} should expose origin_scope");
        for legacy_prop in ["source_agent", "source_conversation", "scope_keys_any", "origin_conversation"] {
            assert!(!props.contains(&legacy_prop.to_owned()), "{admin_tool} should not expose {legacy_prop}");
        }
    }

    let consolidate_props = find_properties("admin_consolidate");
    assert!(consolidate_props.contains(&"scope".to_owned()));
    assert!(consolidate_props.contains(&"scopes".to_owned()));
    assert!(!consolidate_props.contains(&"scope_keys_any".to_owned()));

    let reassign_props = find_properties("admin_reassign_scope");
    assert!(reassign_props.contains(&"origin_scope".to_owned()));
    assert!(!reassign_props.contains(&"origin_conversation".to_owned()));

    for admin_tool in [
        "admin_bulk_delete",
        "admin_bulk_update",
        "admin_cleanup_expired",
        "admin_consolidate",
        "admin_count",
        "admin_history",
        "admin_list",
        "admin_reassign_scope",
        "admin_reembed",
    ] {
        assert!(
            !find_properties(admin_tool).contains(&"caller_agent".to_owned()),
            "{admin_tool} must use the server-resolved principal, not caller_agent"
        );
    }
}

#[tokio::test]
async fn tool_metadata_describes_shorthand_and_object_inputs() {
    let client = setup_noop_server().await;
    let tools = client.list_all_tools().await.unwrap();

    let find_tool = |name: &str| {
        #[expect(clippy::panic, reason = "test assertion with formatted message")]
        tools.iter().find(|tool| tool.name == name).unwrap_or_else(|| panic!("tool {name} not found"))
    };
    let schema_text = |name: &str| serde_json::to_string(&find_tool(name).input_schema).unwrap();

    let remember_many = schema_text("remember_many");
    assert!(remember_many.contains("string content shorthand"));
    assert!(remember_many.contains("full `remember` object"));

    let handoff = schema_text("handoff");
    assert!(handoff.contains("string content shorthand"));
    assert!(handoff.contains("full candidate object"));

    let remember = schema_text("remember");
    assert!(remember.contains("string name shorthand"));
    assert!(remember.contains("full `{name, type}` object"));
    assert!(remember.contains("public"));
    assert!(remember.contains("full policy object"));
}

#[tokio::test]
async fn read_many_metadata_describes_ordering_partial_results_and_activity() {
    let client = setup_noop_server().await;
    let tools = client.list_all_tools().await.unwrap();
    let tool = tools.iter().find(|tool| tool.name == "read_many").unwrap();
    let description = tool.description.as_deref().unwrap_or_default();
    let schema = serde_json::to_string(&tool.input_schema).unwrap();

    assert!(description.contains("Preserves input order"));
    assert!(description.contains("per-item not_found"));
    assert!(description.contains("max_batch_size"));
    assert!(description.contains("trusted principals"));
    assert!(schema.contains("Order is preserved"));
    assert!(schema.contains("limits.max_batch_size"));
}

#[tokio::test]
async fn admin_tool_metadata_describes_authorization_and_preview_behavior() {
    let client = setup_noop_server().await;
    let tools = client.list_all_tools().await.unwrap();
    let description = |name: &str| {
        #[expect(clippy::panic, reason = "test assertion with formatted message")]
        tools
            .iter()
            .find(|tool| tool.name == name)
            .unwrap_or_else(|| panic!("tool {name} not found"))
            .description
            .as_deref()
            .unwrap_or_default()
            .to_owned()
    };

    for name in [
        "admin_bulk_delete",
        "admin_bulk_update",
        "admin_cleanup_expired",
        "admin_consolidate",
        "admin_count",
        "admin_history",
        "admin_list",
        "admin_reassign_scope",
        "admin_reembed",
        "admin_scope_list",
        "admin_scope_register",
        "admin_v2_migrate_metadata",
        "admin_v2_migration_report",
    ] {
        assert!(description(name).contains("server-resolved principal"), "{name} should mention server-resolved principal");
    }
    assert!(description("admin_list").contains("Read-like admin"));
    assert!(description("admin_count").contains("Read-like admin"));
    assert!(description("admin_bulk_delete").contains("destructively delete"));
    assert!(description("admin_consolidate").contains("dry_run=true previews"));
    assert!(description("admin_consolidate").contains("dry_run=false merges"));
    assert!(description("admin_v2_migrate_metadata").contains("dry_run=true previews"));
}

#[tokio::test]
async fn store_roundtrip_through_protocol() {
    let client = setup_noop_server().await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "protocol roundtrip test",
            "tags": ["integration", "protocol"],
            "agent_label": "test-bot"
        }),
    )
    .await;
    assert!(!remembered.id.to_string().is_empty());

    let read: ReadResponse = call_tool(&client, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.memory.content, "protocol roundtrip test");
    assert_eq!(read.memory.tags, vec!["integration", "protocol"]);
    assert_eq!(read.agent_label.as_deref(), Some("test-bot"));
}

#[tokio::test]
async fn v2_read_nonexistent_memory_returns_not_found() {
    let client = setup_noop_server().await;
    let fake_id = "01J0000000000000000000000A";

    let err = call_tool_error(&client, "read", json!({"id": fake_id})).await;
    assert!(err.contains("not found"), "expected not-found error, got: {err}");
    let structured = parse_tool_error(&err);
    assert_eq!(structured.error.code, ToolErrorCode::NotFound);
    assert_eq!(structured.error.field.as_deref(), Some("id"));
    assert!(!structured.error.retryable);
}

#[tokio::test]
async fn v2_remember_invalid_content_returns_structured_tool_error() {
    let client = setup_noop_server().await;

    let err = call_tool_error(&client, "remember", json!({"content": "   "})).await;
    let structured = parse_tool_error(&err);

    assert_eq!(structured.error.code, ToolErrorCode::InvalidParams);
    assert_eq!(structured.error.field.as_deref(), Some("content"));
    assert!(structured.error.message.contains("blank"));
    assert!(!structured.error.retryable);
}

#[tokio::test]
async fn v2_recall_empty_store_returns_no_results() {
    let client = setup_noop_server().await;

    let recalled: RecallResponse = call_tool(&client, "recall", json!({"query": "nonexistent content"})).await;
    assert_eq!(recalled.count, 0_usize);
    assert!(recalled.results.is_empty());
    assert_eq!(recalled.weak_result_count, 0_usize);
}

#[tokio::test]
async fn v2_entities_roundtrip_filter_update_and_delete() {
    let client = setup_noop_server().await;

    let target: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "Alice works on project X",
            "scope": "entity/protocol",
            "entities": [
                {"name": "Alice", "type": "person"},
                {"name": "project X", "type": "project"}
            ]
        }),
    )
    .await;
    let distractor: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "Bob does other things",
            "scope": "entity/protocol",
            "entities": [{"name": "Bob", "type": "person"}]
        }),
    )
    .await;

    let read: ReadResponse = call_tool(&client, "read", json!({"id": target.id})).await;
    assert_eq!(read.memory.entities.len(), 2);
    assert!(read.memory.entities.iter().any(|entity| entity.name == "Alice" && entity.entity_type.as_str() == "person"));
    assert!(
        read.memory
            .entities
            .iter()
            .any(|entity| entity.name == "project X" && entity.entity_type.as_str() == "project")
    );

    let by_entity: AdminListResponse = call_tool(&client, "admin_list", json!({"entity": "Alice"})).await;
    assert_eq!(by_entity.count, 1);
    assert_eq!(by_entity.memories[0].id, target.id);

    let by_type: AdminListResponse = call_tool(&client, "admin_list", json!({"entity_type": "project"})).await;
    assert_eq!(by_type.count, 1);
    assert_eq!(by_type.memories[0].id, target.id);

    let revised: UpdateResponse = call_tool(
        &client,
        "revise",
        json!({
            "id": target.id,
            "entities": [{"name": "Carol", "type": "person"}]
        }),
    )
    .await;
    assert!(revised.updated);

    let updated: ReadResponse = call_tool(&client, "read", json!({"id": target.id})).await;
    assert_eq!(updated.memory.entities.len(), 1);
    assert_eq!(updated.memory.entities[0].name, "Carol");

    let old_entity: AdminListResponse = call_tool(&client, "admin_list", json!({"entity": "Alice"})).await;
    assert_eq!(old_entity.count, 0);
    let new_entity: AdminListResponse = call_tool(&client, "admin_list", json!({"entity": "Carol"})).await;
    assert_eq!(new_entity.count, 1);
    assert_eq!(new_entity.memories[0].id, target.id);

    let _deleted: serde_json::Value = call_tool(&client, "forget", json!({"id": target.id})).await;

    let after_delete: AdminListResponse = call_tool(&client, "admin_list", json!({"entity": "Carol"})).await;
    assert_eq!(after_delete.count, 0);
    let bob: AdminListResponse = call_tool(&client, "admin_list", json!({"entity": "Bob"})).await;
    assert_eq!(bob.count, 1);
    assert_eq!(bob.memories[0].id, distractor.id);
}

#[tokio::test]
async fn v2_entity_string_shorthand_roundtrips_across_write_tools() {
    let client = setup_noop_server().await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "entity shorthand remember durable fact",
            "scope": "entity/shorthand",
            "entities": ["shortcut project"]
        }),
    )
    .await;
    let read: ReadResponse = call_tool(&client, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.memory.entities.len(), 1_usize);
    assert_eq!(read.memory.entities[0].name, "shortcut project");
    assert_eq!(read.memory.entities[0].entity_type.as_str(), "unknown");

    let remembered_many: RememberManyResponse = call_tool(
        &client,
        "remember_many",
        json!({
            "memories": [{
                "content": "entity shorthand batch durable fact",
                "scope": "entity/shorthand",
                "entities": ["batch entity"]
            }]
        }),
    )
    .await;
    let batch_read: ReadResponse = call_tool(&client, "read", json!({"id": remembered_many.memories[0].id})).await;
    assert_eq!(batch_read.memory.entities[0].name, "batch entity");
    assert_eq!(batch_read.memory.entities[0].entity_type.as_str(), "unknown");

    let revised: UpdateResponse = call_tool(
        &client,
        "revise",
        json!({
            "id": remembered.id,
            "entities": ["revised entity"]
        }),
    )
    .await;
    assert!(revised.updated);
    let revised_read: ReadResponse = call_tool(&client, "read", json!({"id": remembered.id})).await;
    assert_eq!(revised_read.memory.entities[0].name, "revised entity");
    assert_eq!(revised_read.memory.entities[0].entity_type.as_str(), "unknown");

    let handoff: HandoffResponse = call_tool(
        &client,
        "handoff",
        json!({
            "commit": true,
            "candidates": [{
                "content": "entity shorthand handoff durable fact",
                "scope": "entity/shorthand",
                "entities": ["handoff entity"]
            }]
        }),
    )
    .await;
    assert!(handoff.committed);
    assert!(handoff.suggested_writes[0].id.is_some(), "committed handoff should return an id");
    if let Some(handoff_id) = handoff.suggested_writes[0].id {
        let handoff_read: ReadResponse = call_tool(&client, "read", json!({"id": handoff_id})).await;
        assert_eq!(handoff_read.memory.entities[0].name, "handoff entity");
        assert_eq!(handoff_read.memory.entities[0].entity_type.as_str(), "unknown");
    }
}

#[tokio::test]
async fn v2_access_policy_public_shorthand_roundtrips_across_write_tools() {
    let (client, server) = setup_server_with_auth(Arc::new(NoopEmbedding::new()), Some("owner"), AnonymousPolicy::PublicReadOnly).await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "public access policy shorthand remember durable fact",
            "scope": "access-policy/shorthand",
            "access_policy": "public"
        }),
    )
    .await;
    let remembered_memory = server.store().get(&remembered.id, None).await.unwrap();
    assert!(matches!(remembered_memory.as_ref().map(|memory| &memory.access_policy), Some(AccessPolicy::Public)));

    let remembered_many: RememberManyResponse = call_tool(
        &client,
        "remember_many",
        json!({
            "memories": [{
                "content": "public access policy shorthand batch durable fact",
                "scope": "access-policy/shorthand",
                "access_policy": "public"
            }]
        }),
    )
    .await;
    let batch_memory = server.store().get(&remembered_many.memories[0].id, None).await.unwrap();
    assert!(matches!(batch_memory.as_ref().map(|memory| &memory.access_policy), Some(AccessPolicy::Public)));

    let restricted: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "restricted memory that revise will publish",
            "scope": "access-policy/shorthand",
            "access_policy": {"type": "restricted", "allowed": ["owner"]}
        }),
    )
    .await;
    assert!(server.store().get(&restricted.id, None).await.unwrap().is_none());

    let revised: UpdateResponse = call_tool(
        &client,
        "revise",
        json!({
            "id": restricted.id,
            "access_policy": "public"
        }),
    )
    .await;
    assert!(revised.updated);
    let revised_memory = server.store().get(&restricted.id, None).await.unwrap();
    assert!(matches!(revised_memory.as_ref().map(|memory| &memory.access_policy), Some(AccessPolicy::Public)));

    let bulk_target: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "restricted memory that bulk update will publish",
            "scope": "access-policy/shorthand",
            "tags": ["bulk-access-policy-shorthand"],
            "access_policy": {"type": "restricted", "allowed": ["owner"]}
        }),
    )
    .await;
    assert!(server.store().get(&bulk_target.id, None).await.unwrap().is_none());

    let bulk: BulkUpdateResponse = call_tool(
        &client,
        "admin_bulk_update",
        json!({
            "tags": ["bulk-access-policy-shorthand"],
            "access_policy": "public"
        }),
    )
    .await;
    assert_eq!(bulk.matched, 1);
    assert_eq!(bulk.updated, 1);
    let bulk_memory = server.store().get(&bulk_target.id, None).await.unwrap();
    assert!(matches!(bulk_memory.as_ref().map(|memory| &memory.access_policy), Some(AccessPolicy::Public)));
}

#[tokio::test]
async fn v2_admin_bulk_update_and_delete_filter_workflow() {
    let client = setup_noop_server().await;

    let keep: RememberResponse = call_tool(&client, "remember", json!({"content": "keep me", "tags": ["keep"]})).await;
    let delete_alpha: RememberResponse = call_tool(&client, "remember", json!({"content": "delete alpha", "tags": ["delete-me"]})).await;
    let delete_beta: RememberResponse = call_tool(&client, "remember", json!({"content": "delete beta", "tags": ["delete-me"]})).await;
    let update_alpha: RememberResponse = call_tool(&client, "remember", json!({"content": "update alpha", "tags": ["update-me"]})).await;
    let update_beta: RememberResponse = call_tool(&client, "remember", json!({"content": "update beta", "tags": ["update-me"]})).await;

    let updated: BulkUpdateResponse = call_tool(
        &client,
        "admin_bulk_update",
        json!({
            "tags": ["update-me"],
            "importance": 0.9_f64
        }),
    )
    .await;
    assert_eq!(updated.matched, 2);
    assert_eq!(updated.updated, 2);
    assert_eq!(updated.denied, 0);

    for id in [update_alpha.id, update_beta.id] {
        let read: ReadResponse = call_tool(&client, "read", json!({"id": id})).await;
        assert!((read.memory.importance - 0.9_f64).abs() < f64::EPSILON);
    }

    let deleted: BulkDeleteResponse = call_tool(
        &client,
        "admin_bulk_delete",
        json!({
            "tags": ["delete-me"]
        }),
    )
    .await;
    assert_eq!(deleted.matched, 2);
    assert_eq!(deleted.deleted, 2);

    for id in [delete_alpha.id, delete_beta.id] {
        let err = call_tool_error(&client, "read", json!({"id": id})).await;
        assert!(err.contains("not found"), "expected deleted memory to be gone, got: {err}");
    }

    let listed: AdminListResponse = call_tool(&client, "admin_list", json!({})).await;
    assert_eq!(listed.count, 3);
    assert!(listed.memories.iter().any(|memory| memory.id == keep.id));
    assert!(listed.memories.iter().any(|memory| memory.id == update_alpha.id));
    assert!(listed.memories.iter().any(|memory| memory.id == update_beta.id));
}

#[tokio::test]
async fn v2_admin_tools_reject_removed_wire_names() {
    let client = setup_noop_server().await;

    for (tool, args, expected) in [
        ("admin_list", json!({"source_agent": "bot"}), "agent_label"),
        ("admin_count", json!({"source_conversation": "scope"}), "scope"),
        ("admin_bulk_update", json!({"origin_conversation": "scope"}), "origin_scope"),
        ("admin_bulk_delete", json!({"scope_keys_any": ["scope"]}), "scopes"),
        ("admin_consolidate", json!({"scope_keys_any": ["scope"]}), "scopes"),
        (
            "admin_reassign_scope",
            json!({"from_scope": "old", "to_scope": "new", "origin_conversation": "old"}),
            "origin_scope",
        ),
    ] {
        assert_invalid_params_contains(&client, tool, args, expected).await;
    }
}

#[tokio::test]
async fn v2_admin_history_reports_audit_entries_without_memory_content() {
    let (client, server) = setup_server_with(Arc::new(NoopEmbedding::new())).await;

    let lifecycle: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "audit lifecycle original content",
            "tags": ["audit-history"],
            "agent_label": "audit-agent"
        }),
    )
    .await;
    let _updated: UpdateResponse = call_tool(
        &client,
        "revise",
        json!({
            "id": lifecycle.id,
            "content": "audit lifecycle updated content",
            "tags": ["audit-history", "updated"]
        }),
    )
    .await;
    let _deleted: serde_json::Value = call_tool(&client, "forget", json!({"id": lifecycle.id})).await;

    let bulk_target: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "audit bulk delete content",
            "tags": ["audit-bulk-delete"]
        }),
    )
    .await;
    let bulk_deleted: BulkDeleteResponse = call_tool(&client, "admin_bulk_delete", json!({"tags": ["audit-bulk-delete"]})).await;
    assert_eq!(bulk_deleted.deleted, 1_u64);

    server.shutdown_for_test(Duration::from_secs(2)).await;

    let lifecycle_history: HistoryResponse = call_tool(&client, "admin_history", json!({"id": lifecycle.id, "limit": 50_i32})).await;
    let lifecycle_actions = lifecycle_history.entries.iter().map(|entry| entry.action).collect::<Vec<_>>();
    assert!(lifecycle_actions.contains(&AuditAction::Store), "should record store entry");
    assert!(lifecycle_actions.contains(&AuditAction::Update), "should record update entry");
    assert!(lifecycle_actions.contains(&AuditAction::Delete), "should record delete entry");

    let update_entry = lifecycle_history.entries.iter().find(|entry| entry.action == AuditAction::Update);
    assert!(update_entry.is_some(), "update audit entry should exist");
    let details = update_entry.and_then(|entry| entry.details.as_ref());
    assert!(details.is_some(), "update audit entry should include details");
    let details = details.unwrap_or(&serde_json::Value::Null);
    assert!(details.get("old_content_hash").is_some(), "update details should contain old_content_hash");
    let history_json = serde_json::to_string(&lifecycle_history);
    assert!(history_json.is_ok(), "history should serialize: {history_json:?}");
    assert!(
        !history_json.unwrap_or_default().contains("audit lifecycle updated content"),
        "admin_history must not return raw memory content"
    );

    let bulk_history: HistoryResponse = call_tool(&client, "admin_history", json!({"id": bulk_target.id})).await;
    let bulk_actions = bulk_history.entries.iter().map(|entry| entry.action).collect::<Vec<_>>();
    assert!(bulk_actions.contains(&AuditAction::Store), "should record store entry");
    assert!(bulk_actions.contains(&AuditAction::BulkDelete), "should record bulk delete entry");

    let empty: HistoryResponse = call_tool(&client, "admin_history", json!({"id": "01J0000000000000000000000A"})).await;
    assert!(empty.entries.is_empty(), "nonexistent memory should have no history entries");
}

#[tokio::test]
async fn v2_admin_history_hides_current_memory_not_visible_to_principal() {
    let store = SqliteStore::in_memory().unwrap();
    let memory = Memory::new_for_test(
        "restricted audit history content".to_owned(),
        vec!["audit-history".to_owned()],
        Provenance::new_for_test(Some("owner".to_owned()), Some("audit/history".to_owned()), None),
        AccessPolicy::Restricted {
            allowed: vec!["friend".to_owned()],
        },
    );
    let id = store.store(&memory, None).await.unwrap();
    store.write_audit_entry(&id, AuditAction::Store, Some("owner"), Utc::now(), None).await.unwrap();

    let engine = RecallEngine::new(store, Arc::new(NoopEmbedding::new()), LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine_with_auth(engine, Some("other".to_owned()), AnonymousPolicy::PublicReadOnly);
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let _server_task = tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });
    let client = ().serve(client_transport).await.unwrap();

    let history: HistoryResponse = call_tool(&client, "admin_history", json!({"id": id})).await;
    assert!(
        history.entries.is_empty(),
        "admin_history must hide entries for memories invisible to the resolved principal"
    );
}

#[tokio::test]
async fn v2_admin_consolidate_previews_and_validates_thresholds() {
    let client = setup_noop_server().await;

    let preview: ConsolidateResponse = call_tool(
        &client,
        "admin_consolidate",
        json!({
            "dry_run": true,
            "similarity_threshold": 0.85_f64
        }),
    )
    .await;
    assert_eq!(preview.operation.status, OperationStatus::Preview);
    assert!(!preview.merged);
    assert!(preview.groups.is_empty());

    assert_invalid_params_contains(
        &client,
        "admin_consolidate",
        json!({
            "similarity_threshold": 1.5_f64
        }),
        "similarity_threshold",
    )
    .await;
}

#[tokio::test]
async fn v2_remember_recall_read_roundtrip_uses_compact_cards() {
    let client = setup_noop_server().await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "v2 durable project decision: compact recall cards omit full content",
            "summary": "Compact cards omit full content",
            "scope": "gearboxlogic/localhold",
            "agent_label": "Protocol Test Agent",
            "tags": ["decision"],
            "entities": [{"name": "localhold", "type": "project"}]
        }),
    )
    .await;
    assert!(!remembered.unresolved_scope, "explicit scope should not be unresolved");
    assert_eq!(remembered.scope_resolution.resolved_by, ScopeResolvedBy::Explicit);
    assert!(remembered.warnings.iter().all(|warning| warning.code != "missing_scope"));

    let recalled: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "compact recall cards",
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;
    assert_eq!(recalled.count, 1_usize);
    assert_eq!(recalled.scope_resolution.as_ref().map(|resolution| resolution.resolved_by), Some(ScopeResolvedBy::Explicit));
    assert_eq!(recalled.results[0].id, remembered.id);
    assert_eq!(recalled.results[0].summary_or_excerpt, "Compact cards omit full content");
    assert_eq!(recalled.results[0].agent_label.as_deref(), Some("Protocol Test Agent"));
    assert_eq!(recalled.results[0].r#match.quality, MatchQuality::Strong);
    assert_eq!(recalled.results[0].r#match.action, MatchAction::Read);
    assert!(recalled.results[0].r#match.score >= 0.5_f64);
    assert!(recalled.results[0].diagnostics.retrieval_score.is_some());

    let read: ReadResponse = call_tool(&client, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.memory.id, remembered.id);
    assert_eq!(read.memory.content, "v2 durable project decision: compact recall cards omit full content");
    assert!(read.activity_recorded, "read should record activity automatically");
}

#[tokio::test]
async fn v2_read_many_preserves_order_and_returns_not_found_items() {
    let client = setup_noop_server().await;
    let first: RememberResponse = call_tool(&client, "remember", json!({"content": "read_many first", "scope": "read-many"})).await;
    let second: RememberResponse = call_tool(&client, "remember", json!({"content": "read_many second", "scope": "read-many"})).await;
    let missing = "01J0000000000000000000000B";

    let batch: ReadManyResponse = call_tool(&client, "read_many", json!({"ids": [second.id, missing, first.id]})).await;

    assert_eq!(batch.results.len(), 3_usize);
    assert_eq!(batch.results[0].id, second.id);
    assert_eq!(batch.results[0].status, ReadManyStatus::Found);
    assert_eq!(batch.results[0].memory.as_ref().map(|memory| memory.content.as_str()), Some("read_many second"));
    assert!(batch.results[0].activity_recorded);
    assert_eq!(batch.results[1].id.to_string(), missing);
    assert_eq!(batch.results[1].status, ReadManyStatus::NotFound);
    assert!(batch.results[1].memory.is_none());
    assert!(!batch.results[1].activity_recorded);
    assert_eq!(batch.results[2].id, first.id);
    assert_eq!(batch.results[2].status, ReadManyStatus::Found);
    assert_eq!(batch.results[2].memory.as_ref().map(|memory| memory.content.as_str()), Some("read_many first"));
    assert!(batch.results[2].activity_recorded);
}

#[tokio::test]
async fn v2_read_many_validates_batch_size() {
    let client = setup_noop_server().await;

    let empty_err = call_tool_error(&client, "read_many", json!({"ids": []})).await;
    let empty = parse_tool_error(&empty_err);
    assert_eq!(empty.error.code, ToolErrorCode::InvalidParams);
    assert_eq!(empty.error.field.as_deref(), Some("ids"));

    let ids = vec!["01J0000000000000000000000C"; 101];
    let oversized_err = call_tool_error(&client, "read_many", json!({"ids": ids})).await;
    let oversized = parse_tool_error(&oversized_err);
    assert_eq!(oversized.error.code, ToolErrorCode::InvalidParams);
    assert_eq!(oversized.error.field.as_deref(), Some("ids"));
}

#[tokio::test]
async fn v2_remember_many_returns_per_item_results_and_operation_summary() {
    let client = setup_noop_server().await;

    let remembered: RememberManyResponse = call_tool(
        &client,
        "remember_many",
        json!({
            "memories": [
                {
                    "content": "remember_many first durable fact",
                    "summary": "First batch fact",
                    "scope": "gearboxlogic/localhold",
                    "tags": ["batch"]
                },
                {
                    "content": "remember_many second durable fact",
                    "summary": "Second batch fact",
                    "scope": "gearboxlogic/localhold",
                    "tags": ["batch"]
                }
            ]
        }),
    )
    .await;

    assert_eq!(remembered.operation.status, OperationStatus::Applied);
    assert_eq!(remembered.operation.changed, 2);
    assert_eq!(remembered.memories.len(), 2);
    assert!(remembered.memories.iter().all(|memory| memory.scope == "gearboxlogic/localhold"));

    let recalled: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "remember_many durable fact",
            "tags": ["batch"]
        }),
    )
    .await;
    assert_eq!(recalled.count, 2);
}

#[tokio::test]
async fn v2_remember_many_accepts_string_shorthand_items() {
    let client = setup_noop_server().await;

    let remembered: RememberManyResponse = call_tool(
        &client,
        "remember_many",
        json!({
            "memories": [
                "remember_many shorthand first durable fact",
                "remember_many shorthand second durable fact"
            ]
        }),
    )
    .await;

    assert_eq!(remembered.operation.status, OperationStatus::Applied);
    assert_eq!(remembered.operation.changed, 2_u64);
    assert_eq!(remembered.memories.len(), 2_usize);
    assert!(remembered.memories.iter().all(|memory| memory.scope == "inbox/unresolved"));
    assert!(remembered.memories.iter().all(|memory| memory.unresolved_scope));

    let first: ReadResponse = call_tool(&client, "read", json!({"id": remembered.memories[0].id})).await;
    assert_eq!(first.memory.content, "remember_many shorthand first durable fact");
}

#[tokio::test]
async fn v2_admin_list_returns_compact_inventory_cards() {
    let client = setup_noop_server().await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "admin_list should not return full memory content in inventory cards",
            "summary": "Inventory card summary",
            "scope": "gearboxlogic/localhold/admin",
            "agent_label": "Inventory Agent",
            "tags": ["inventory"]
        }),
    )
    .await;

    let inventory: AdminListResponse = call_tool(
        &client,
        "admin_list",
        json!({
            "tags": ["inventory"]
        }),
    )
    .await;

    assert_eq!(inventory.count, 1);
    assert_eq!(inventory.memories[0].id, remembered.id);
    assert_eq!(inventory.memories[0].summary_or_excerpt, "Inventory card summary");
    assert_eq!(inventory.memories[0].scope, "gearboxlogic/localhold/admin");
    assert_eq!(inventory.memories[0].agent_label.as_deref(), Some("Inventory Agent"));
}

#[tokio::test]
async fn v2_recall_records_search_impressions() {
    let clock = Arc::new(MockClock::new());
    let (client, server) = setup_noop_server_with_clock(Arc::clone(&clock)).await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "v2 recall impression activity candidate",
            "summary": "Recall impression candidate",
            "scope": "gearboxlogic/localhold",
            "tags": ["wip"]
        }),
    )
    .await;

    let before: ReadResponse = call_tool(&client, "read", json!({"id": remembered.id})).await;
    assert_eq!(before.memory.impression_count, 0);
    assert!(before.memory.last_impressed_at.is_none());

    let recall: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "v2 recall impression activity candidate",
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;
    assert_eq!(recall.count, 1_usize);
    assert_eq!(recall.results[0].id, remembered.id);

    await_embeddings(&server, Duration::from_secs(1)).await;

    let after: ReadResponse = call_tool(&client, "read", json!({"id": remembered.id})).await;
    assert_eq!(after.memory.impression_count, 1);
    assert!(after.memory.last_impressed_at.is_some());
}

#[tokio::test]
async fn v2_recall_suppresses_weak_matches_unless_requested() {
    let (client, server) = setup_embedding_server().await;

    for content in [
        "semantic weak suppression alpha planning note",
        "semantic weak suppression beta unrelated note",
        "semantic weak suppression gamma distant note",
    ] {
        let _remembered: RememberResponse = call_tool(
            &client,
            "remember",
            json!({
                "content": content,
                "summary": content,
                "scope": "gearboxlogic/localhold",
                "tags": ["weak-recall"]
            }),
        )
        .await;
    }
    await_embeddings(&server, Duration::from_secs(1)).await;

    let filtered: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "orthogonal semantic probe",
            "scope": "gearboxlogic/localhold",
            "tags": ["weak-recall"],
            "search_mode": "semantic",
            "limit": 3_i32
        }),
    )
    .await;
    assert!(filtered.weak_result_count > 0, "semantic recall should suppress at least one weak candidate");
    assert!(filtered.results.iter().all(|card| card.r#match.quality != MatchQuality::Weak));

    let with_weak: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "orthogonal semantic probe",
            "scope": "gearboxlogic/localhold",
            "tags": ["weak-recall"],
            "search_mode": "semantic",
            "include_weak": true,
            "limit": 3_i32
        }),
    )
    .await;
    assert_eq!(with_weak.count, filtered.count + filtered.weak_result_count);
    assert!(with_weak.results.iter().any(|card| card.r#match.quality == MatchQuality::Weak));
    assert!(with_weak.results.iter().any(|card| card.r#match.action == MatchAction::Ignore));
}

#[tokio::test]
async fn v2_remember_warns_when_duplicate_candidates_exist() {
    let client = setup_noop_server().await;
    let content = "v2 duplicate warning should surface similar existing memories";

    let _first: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": content,
            "summary": "Duplicate warning baseline",
            "scope": "gearboxlogic/localhold",
            "tags": ["decision"]
        }),
    )
    .await;

    let second: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": content,
            "summary": "Duplicate warning duplicate",
            "scope": "gearboxlogic/localhold",
            "tags": ["decision"]
        }),
    )
    .await;

    assert!(!second.duplicate_candidates.is_empty());
    assert!(second.duplicate_candidates.iter().all(|candidate| candidate.r#match.action == MatchAction::Read));
    let duplicate_warning = second.warnings.iter().find(|warning| warning.code == "duplicate_candidate").unwrap();
    assert_eq!(duplicate_warning.severity, QualityWarningSeverity::Warning);
    assert_eq!(duplicate_warning.field.as_deref(), Some("content"));
    assert!(duplicate_warning.suggested_fix.is_some());
}

#[tokio::test]
async fn v2_remember_quality_warnings_are_advisory() {
    let client = setup_noop_server().await;
    let content = format!("{}\n```rust\nfn derived_from_code() {{}}\n```", "oversized agent memory candidate ".repeat(160));

    let remembered: RememberResponse = call_tool(&client, "remember", json!({ "content": content })).await;

    assert_eq!(remembered.scope, "inbox/unresolved");
    assert!(remembered.unresolved_scope);
    assert_warning_codes(&remembered.warnings, &[
        "missing_scope",
        "missing_summary",
        "empty_tags",
        "empty_entities",
        "oversized_content",
        "possible_code_dump",
    ]);
    let missing_scope = remembered.warnings.iter().find(|warning| warning.code == "missing_scope").unwrap();
    assert_eq!(missing_scope.severity, QualityWarningSeverity::ActionRequired);
    assert_eq!(missing_scope.field.as_deref(), Some("scope"));
    assert!(missing_scope.suggested_fix.is_some());
    let empty_tags = remembered.warnings.iter().find(|warning| warning.code == "empty_tags").unwrap();
    assert_eq!(empty_tags.severity, QualityWarningSeverity::Info);
    assert_eq!(empty_tags.field.as_deref(), Some("tags"));
    let code_dump = remembered.warnings.iter().find(|warning| warning.code == "possible_code_dump").unwrap();
    assert_eq!(code_dump.severity, QualityWarningSeverity::Warning);
    assert_eq!(code_dump.field.as_deref(), Some("content"));

    let read: ReadResponse = call_tool(&client, "read", json!({ "id": remembered.id })).await;
    assert_eq!(read.memory.content, content);
}

#[tokio::test]
async fn v2_revise_updates_card_metadata() {
    let client = setup_noop_server().await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "v2 revise metadata keeps cards current",
            "summary": "Old revise summary",
            "agent_label": "Old Agent",
            "scope": "gearboxlogic/localhold",
            "tags": ["decision"]
        }),
    )
    .await;

    let revised: UpdateResponse = call_tool(
        &client,
        "revise",
        json!({
            "id": remembered.id,
            "summary": "New revise summary",
            "agent_label": "New Agent"
        }),
    )
    .await;
    assert!(revised.updated);

    let recalled: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "v2 revise metadata keeps cards current",
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;

    assert_eq!(recalled.count, 1_usize);
    assert_eq!(recalled.results[0].id, remembered.id);
    assert_eq!(recalled.results[0].summary_or_excerpt, "New revise summary");
    assert_eq!(recalled.results[0].agent_label.as_deref(), Some("New Agent"));
}

#[tokio::test]
async fn v2_revise_classifies_unresolved_memory_from_context_hints() {
    let client = setup_noop_server().await;

    let _registered: serde_json::Value = call_tool(
        &client,
        "admin_scope_register",
        json!({
            "scope_key": "gearboxlogic/localhold",
            "display_name": "LocalHold",
            "matchers": ["/workspace/localhold"]
        }),
    )
    .await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "revise should classify unresolved scope later",
            "summary": "Unresolved revise classification",
            "tags": ["wip"]
        }),
    )
    .await;
    assert_eq!(remembered.scope, "inbox/unresolved");

    let revised: UpdateResponse = call_tool(
        &client,
        "revise",
        json!({
            "id": remembered.id,
            "context_hints": ["/workspace/localhold/src/server/mod.rs"]
        }),
    )
    .await;
    assert!(revised.updated);
    assert_eq!(revised.scope_resolution.as_ref().map(|resolution| resolution.resolved_by), Some(ScopeResolvedBy::Matcher));

    let read: ReadResponse = call_tool(&client, "read", json!({"id": remembered.id})).await;
    assert_eq!(read.scope.as_deref(), Some("gearboxlogic/localhold"));

    let recalled: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "revise should classify unresolved scope later",
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;

    assert_eq!(recalled.count, 1_usize);
    assert_eq!(recalled.results[0].id, remembered.id);
    assert_eq!(recalled.results[0].scope, "gearboxlogic/localhold");
}

#[tokio::test]
async fn v2_missing_scope_lands_in_unresolved_inbox() {
    let client = setup_noop_server().await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "v2 unresolved memory should be classified later"
        }),
    )
    .await;

    assert_eq!(remembered.scope, "inbox/unresolved");
    assert!(remembered.unresolved_scope);
    assert_eq!(remembered.scope_resolution.resolved_by, ScopeResolvedBy::Unresolved);
    assert!(remembered.warnings.iter().any(|warning| warning.code == "missing_scope"));
}

#[tokio::test]
async fn v2_scope_registry_resolves_aliases_and_matchers() {
    let client = setup_noop_server().await;

    let _registered: serde_json::Value = call_tool(
        &client,
        "admin_scope_register",
        json!({
            "scope_key": "gearboxlogic/localhold",
            "display_name": "LocalHold",
            "aliases": ["recall"],
            "matchers": ["/workspace/localhold"]
        }),
    )
    .await;

    let alias_write: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "scope aliases resolve to canonical keys",
            "scope": "recall"
        }),
    )
    .await;
    assert_eq!(alias_write.scope, "gearboxlogic/localhold");
    assert_eq!(alias_write.scope_resolution.resolved_by, ScopeResolvedBy::Alias);
    assert_eq!(alias_write.scope_resolution.matched_hint.as_deref(), Some("recall"));

    let matcher_write: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "scope matchers resolve from path context",
            "scope": "/workspace/localhold/src/server/mod.rs"
        }),
    )
    .await;
    assert_eq!(matcher_write.scope, "gearboxlogic/localhold");
    assert_eq!(matcher_write.scope_resolution.resolved_by, ScopeResolvedBy::Matcher);
    assert_eq!(matcher_write.scope_resolution.matched_hint.as_deref(), Some("/workspace/localhold/src/server/mod.rs"));

    let context_hint_write: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "scope matchers resolve from context hints",
            "context_hints": ["/workspace/localhold/Cargo.toml"]
        }),
    )
    .await;
    assert_eq!(context_hint_write.scope, "gearboxlogic/localhold");
    assert!(!context_hint_write.unresolved_scope);
    assert_eq!(context_hint_write.scope_resolution.resolved_by, ScopeResolvedBy::Matcher);
    assert_eq!(context_hint_write.scope_resolution.matched_hint.as_deref(), Some("/workspace/localhold/Cargo.toml"));
    assert!(context_hint_write.warnings.iter().all(|warning| warning.code != "missing_scope"));

    let raw_explicit: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "raw explicit scope stays as supplied",
            "scope": "unregistered/raw-scope"
        }),
    )
    .await;
    assert_eq!(raw_explicit.scope, "unregistered/raw-scope");
    assert_eq!(raw_explicit.scope_resolution.resolved_by, ScopeResolvedBy::Explicit);
    assert!(raw_explicit.scope_resolution.matched_value.is_none());
}

#[tokio::test]
async fn v2_admin_reassign_scope_updates_card_metadata_scope() {
    let client = setup_noop_server().await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "admin reassign scope updates v2 card metadata",
            "summary": "Reassigned card metadata",
            "scope": "old/project",
            "tags": ["decision"]
        }),
    )
    .await;

    let reassigned: ReassignScopeResponse = call_tool(
        &client,
        "admin_reassign_scope",
        json!({
            "from_scope": "old/project",
            "to_scope": "new/project"
        }),
    )
    .await;
    assert_eq!(reassigned.reassigned, 1_u64);

    let recalled: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "admin reassign scope updates v2 card metadata",
            "scope": "new/project"
        }),
    )
    .await;

    assert_eq!(recalled.count, 1_usize);
    assert_eq!(recalled.results[0].id, remembered.id);
    assert_eq!(recalled.results[0].scope, "new/project");
}

#[tokio::test]
async fn v2_admin_reassign_scope_respects_origin_filter_and_records_history() {
    let (client, server) = setup_server_with_auth(Arc::new(NoopEmbedding::new()), Some("bot"), AnonymousPolicy::PublicReadOnly).await;
    let moved_id = seed_reassign_memory(&server, "v2 reassign origin moved", "bot", "project-1", "conv-a").await;
    let retained_id = seed_reassign_memory(&server, "v2 reassign origin retained", "bot", "project-1", "conv-b").await;

    let reassigned: ReassignScopeResponse = call_tool(
        &client,
        "admin_reassign_scope",
        json!({
            "from_scope": "project-1",
            "to_scope": "conv-a",
            "origin_scope": "conv-a"
        }),
    )
    .await;
    assert_eq!(reassigned.reassigned, 1_u64);
    assert_eq!(reassigned.operation.status, OperationStatus::Applied);

    server.shutdown_for_test(Duration::from_secs(2)).await;

    let moved: ReadResponse = call_tool(&client, "read", json!({"id": moved_id})).await;
    assert_eq!(moved.scope.as_deref(), Some("conv-a"));
    let retained: ReadResponse = call_tool(&client, "read", json!({"id": retained_id})).await;
    assert_eq!(retained.scope.as_deref(), Some("project-1"));

    let moved_history: HistoryResponse = call_tool(&client, "admin_history", json!({"id": moved_id, "limit": 10_i32})).await;
    assert!(
        moved_history
            .entries
            .iter()
            .any(|entry| entry.action == AuditAction::Reassign && entry.principal.as_deref() == Some("bot")),
        "moved memory should expose a queryable reassign audit entry"
    );

    let retained_history: HistoryResponse = call_tool(&client, "admin_history", json!({"id": retained_id, "limit": 10_i32})).await;
    assert!(
        retained_history.entries.iter().all(|entry| entry.action != AuditAction::Reassign),
        "non-moved memories should not receive reassign audit entries"
    );
}

#[tokio::test]
async fn v2_admin_reassign_scope_skips_unauthorized_matches() {
    let (client, server) = setup_server_with_auth(Arc::new(NoopEmbedding::new()), Some("owner"), AnonymousPolicy::PublicReadOnly).await;
    let owned_id = seed_reassign_memory(&server, "v2 reassign owned", "owner", "project-1", "conv-a").await;
    let denied_id = seed_reassign_memory(&server, "v2 reassign denied", "other", "project-1", "conv-b").await;

    let reassigned: ReassignScopeResponse = call_tool(
        &client,
        "admin_reassign_scope",
        json!({
            "from_scope": "project-1",
            "to_scope": "project-2"
        }),
    )
    .await;
    assert_eq!(reassigned.reassigned, 1_u64);

    let owned: ReadResponse = call_tool(&client, "read", json!({"id": owned_id})).await;
    assert_eq!(owned.scope.as_deref(), Some("project-2"));

    let denied: ReadResponse = call_tool(&client, "read", json!({"id": denied_id})).await;
    assert_eq!(denied.scope.as_deref(), Some("project-1"));
}

#[tokio::test]
async fn v2_forget_denied_returns_structured_tool_error() {
    let (client, server) = setup_server_with_auth(Arc::new(NoopEmbedding::new()), Some("owner"), AnonymousPolicy::PublicReadOnly).await;
    let denied_id = seed_reassign_memory(&server, "forget denied structured error", "other", "project-1", "conv-a").await;

    let err = call_tool_error(&client, "forget", json!({"id": denied_id})).await;
    let structured = parse_tool_error(&err);

    assert_eq!(structured.error.code, ToolErrorCode::AccessDenied);
    assert_eq!(structured.error.field.as_deref(), Some("id"));
    assert!(structured.error.message.contains("access denied"));
    assert!(!structured.error.retryable);
}

#[tokio::test]
async fn v2_admin_reassign_scope_validates_and_trims_inputs() {
    let (client, server) = setup_server_with_auth(Arc::new(NoopEmbedding::new()), Some("bot"), AnonymousPolicy::PublicReadOnly).await;
    let seeded_id = seed_reassign_memory(&server, "v2 reassign trimmed", "bot", "project-1", "conv-a").await;

    assert_invalid_params_contains(
        &client,
        "admin_reassign_scope",
        json!({
            "from_scope": " ",
            "to_scope": "project-1"
        }),
        "from_scope: cannot be empty when provided",
    )
    .await;
    assert_invalid_params_contains(
        &client,
        "admin_reassign_scope",
        json!({
            "from_scope": "conv-1",
            "to_scope": "conv-1"
        }),
        "from_scope and to_scope must be different",
    )
    .await;

    let moved: ReassignScopeResponse = call_tool(
        &client,
        "admin_reassign_scope",
        json!({
            "from_scope": "  project-1  ",
            "to_scope": "  conv-a  ",
            "origin_scope": "  conv-a  "
        }),
    )
    .await;
    assert_eq!(moved.reassigned, 1_u64);

    let moved_memory: ReadResponse = call_tool(&client, "read", json!({"id": seeded_id})).await;
    assert_eq!(moved_memory.scope.as_deref(), Some("conv-a"));
}

#[tokio::test]
async fn v2_brief_resolves_scope_from_context_hints() {
    let client = setup_noop_server().await;

    let _registered: serde_json::Value = call_tool(
        &client,
        "admin_scope_register",
        json!({
            "scope_key": "gearboxlogic/localhold",
            "display_name": "LocalHold",
            "matchers": ["/workspace/localhold"]
        }),
    )
    .await;

    let in_scope: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "brief context hint scoped decision",
            "summary": "In-scope brief decision",
            "scope": "gearboxlogic/localhold",
            "tags": ["decision"]
        }),
    )
    .await;

    let _out_of_scope: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "brief context hint scoped decision",
            "summary": "Out-of-scope brief decision",
            "scope": "other/project",
            "tags": ["decision"]
        }),
    )
    .await;

    let brief: BriefResponse = call_tool(
        &client,
        "brief",
        json!({
            "query": "brief context hint scoped decision",
            "context_hints": ["/workspace/localhold/src/server/mod.rs"]
        }),
    )
    .await;

    assert_eq!(brief.relevant.len(), 1_usize);
    assert_eq!(brief.relevant[0].id, in_scope.id);
    assert_eq!(brief.relevant[0].scope, "gearboxlogic/localhold");
}

#[tokio::test]
async fn v2_brief_recommends_read_for_one_suggested_read() {
    let client = setup_noop_server().await;
    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "brief single suggested read durable fact",
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;

    let brief: BriefResponse = call_tool(&client, "brief", json!({"query": "brief single suggested read durable fact"})).await;

    assert_eq!(brief.suggested_reads, vec![remembered.id]);
    assert_eq!(brief.recommended_actions.len(), 1_usize);
    let action = &brief.recommended_actions[0];
    assert_eq!(action.tool, RecommendedActionTool::Read);
    assert_eq!(action.priority, RecommendedActionPriority::High);
    assert_eq!(action.arguments, Some(json!({ "id": remembered.id })));
}

#[tokio::test]
async fn v2_brief_recommends_read_many_for_ordered_suggested_reads() {
    let client = setup_noop_server().await;
    let _first: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "brief multiple suggested reads shared topic first",
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;
    let _second: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "brief multiple suggested reads shared topic second",
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;

    let brief: BriefResponse = call_tool(&client, "brief", json!({"query": "brief multiple suggested reads shared topic"})).await;

    assert_eq!(brief.suggested_reads.len(), 2_usize);
    assert_eq!(brief.recommended_actions.len(), 1_usize);
    let action = &brief.recommended_actions[0];
    assert_eq!(action.tool, RecommendedActionTool::ReadMany);
    assert_eq!(action.priority, RecommendedActionPriority::High);
    assert_eq!(action.arguments, Some(json!({ "ids": brief.suggested_reads.clone() })));
}

#[tokio::test]
async fn v2_brief_recommends_scope_registration_without_partial_arguments() {
    let client = setup_noop_server().await;
    let _remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "brief unresolved context hint action",
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;

    let brief: BriefResponse = call_tool(
        &client,
        "brief",
        json!({
            "query": "brief unresolved context hint action",
            "context_hints": ["/no/registered/scope/path"]
        }),
    )
    .await;

    let action = brief.recommended_actions.iter().find(|action| action.tool == RecommendedActionTool::AdminScopeRegister);
    assert!(action.is_some(), "expected admin_scope_register action in {:?}", brief.recommended_actions);
    if let Some(action) = action {
        assert_eq!(action.priority, RecommendedActionPriority::High);
        assert!(action.arguments.is_none());
    }
}

#[tokio::test]
async fn v2_brief_recommends_remember_for_empty_brief() {
    let client = setup_noop_server().await;

    let brief: BriefResponse = call_tool(&client, "brief", json!({"query": "brief empty no visible memories"})).await;

    assert!(brief.relevant.is_empty());
    assert!(brief.stale_candidates.is_empty());
    assert_eq!(brief.recommended_actions.len(), 1_usize);
    assert_eq!(brief.recommended_actions[0].tool, RecommendedActionTool::Remember);
    assert_eq!(brief.recommended_actions[0].priority, RecommendedActionPriority::Normal);
    assert!(brief.recommended_actions[0].arguments.is_none());
}

#[tokio::test]
async fn v2_brief_recommends_include_weak_recall_for_stale_only_query() {
    let (client, server) = setup_low_rerank_server().await;
    let _first: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "brief stale-only weak action first",
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;
    let _second: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "brief stale-only weak action second",
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;
    await_embeddings(&server, Duration::from_secs(2)).await;

    let brief: BriefResponse = call_tool(&client, "brief", json!({"query": "brief stale-only weak action"})).await;

    assert!(brief.relevant.is_empty());
    assert_eq!(brief.stale_candidates.len(), 2_usize);
    assert_eq!(brief.recommended_actions.len(), 1_usize);
    let action = &brief.recommended_actions[0];
    assert_eq!(action.tool, RecommendedActionTool::Recall);
    assert_eq!(action.priority, RecommendedActionPriority::Low);
    assert_eq!(
        action.arguments,
        Some(json!({
            "query": "brief stale-only weak action",
            "include_weak": true
        }))
    );
}

#[tokio::test]
async fn v2_recall_resolves_scope_from_context_hints() {
    let client = setup_noop_server().await;

    let _registered: serde_json::Value = call_tool(
        &client,
        "admin_scope_register",
        json!({
            "scope_key": "gearboxlogic/localhold",
            "display_name": "LocalHold",
            "aliases": ["recall-project"],
            "matchers": ["/workspace/localhold"]
        }),
    )
    .await;

    let in_scope: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "recall context hint scoped memory",
            "summary": "In-scope recall memory",
            "scope": "gearboxlogic/localhold",
            "tags": ["decision"]
        }),
    )
    .await;

    let _out_of_scope: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "recall context hint scoped memory",
            "summary": "Out-of-scope recall memory",
            "scope": "other/project",
            "tags": ["decision"]
        }),
    )
    .await;

    let recalled: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "recall context hint scoped memory",
            "context_hints": ["/workspace/localhold/src/server/mod.rs"]
        }),
    )
    .await;

    assert_eq!(recalled.count, 1_usize);
    assert_eq!(recalled.results[0].id, in_scope.id);
    assert_eq!(recalled.results[0].scope, "gearboxlogic/localhold");

    let alias_recalled: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "recall context hint scoped memory",
            "scope": "recall-project"
        }),
    )
    .await;

    assert_eq!(alias_recalled.count, 1_usize);
    assert_eq!(alias_recalled.results[0].id, in_scope.id);
}

#[tokio::test]
async fn v2_recall_warns_when_context_hints_do_not_resolve_scope() {
    let client = setup_noop_server().await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "recall unresolved context hint still returns visible memory",
            "summary": "Visible unresolved recall hint memory",
            "scope": "gearboxlogic/localhold",
            "tags": ["wip"]
        }),
    )
    .await;

    let recalled: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "recall unresolved context hint still returns visible memory",
            "context_hints": ["/no/registered/scope/path"]
        }),
    )
    .await;

    assert_eq!(recalled.count, 1_usize);
    assert_eq!(recalled.results[0].id, remembered.id);
    assert_eq!(
        recalled.scope_resolution.as_ref().map(|resolution| resolution.resolved_by),
        Some(ScopeResolvedBy::Unresolved)
    );
    let warning = recalled.warnings.iter().find(|warning| warning.code == "unresolved_scope").unwrap();
    assert_eq!(warning.severity, QualityWarningSeverity::Warning);
    assert_eq!(warning.field.as_deref(), Some("context_hints"));
    assert!(warning.suggested_fix.is_some());
}

#[tokio::test]
async fn v2_brief_warns_when_context_hints_do_not_resolve_scope() {
    let client = setup_noop_server().await;

    let remembered: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "brief unresolved context hint still returns visible memory",
            "summary": "Visible unresolved hint memory",
            "scope": "gearboxlogic/localhold",
            "tags": ["wip"]
        }),
    )
    .await;

    let brief: BriefResponse = call_tool(
        &client,
        "brief",
        json!({
            "query": "brief unresolved context hint still returns visible memory",
            "context_hints": ["/no/registered/scope/path"]
        }),
    )
    .await;

    assert_eq!(brief.relevant.len(), 1_usize);
    assert_eq!(brief.relevant[0].id, remembered.id);
    assert_eq!(brief.scope_resolution.as_ref().map(|resolution| resolution.resolved_by), Some(ScopeResolvedBy::Unresolved));
    assert!(brief.warnings.iter().any(|warning| warning.code == "unresolved_scope"));
}

#[tokio::test]
async fn v2_anonymous_public_read_only_allows_public_recall_and_blocks_writes() {
    let (client, ids) = setup_noop_server_with_auth_and_legacy_memories(
        vec![LegacySeed::new("anonymous public read-only can see public v2 recall cards").tags(&["auth"])],
        None,
        AnonymousPolicy::PublicReadOnly,
    )
    .await;
    let stored_id = ids[0];

    let recalled: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "anonymous public read-only",
            "tags": ["auth"]
        }),
    )
    .await;
    assert_eq!(recalled.count, 1_usize);
    assert_eq!(recalled.results[0].id, stored_id);

    let read: ReadResponse = call_tool(&client, "read", json!({"id": stored_id})).await;
    assert_eq!(read.memory.content, "anonymous public read-only can see public v2 recall cards");
    assert!(!read.activity_recorded, "anonymous reads do not create owner activity");

    let read_many: ReadManyResponse = call_tool(&client, "read_many", json!({"ids": [stored_id]})).await;
    assert_eq!(read_many.results[0].status, ReadManyStatus::Found);
    assert!(!read_many.results[0].activity_recorded, "anonymous read_many does not create owner activity");

    let err = call_tool_error(
        &client,
        "remember",
        json!({
            "content": "anonymous write should be denied",
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;
    assert!(err.contains("anonymous writes are disabled"));
    let structured = parse_tool_error(&err);
    assert_eq!(structured.error.code, ToolErrorCode::AnonymousWriteDenied);
    assert!(!structured.error.retryable);

    let err = call_tool_error(&client, "admin_v2_migration_report", json!({})).await;
    assert!(err.contains("anonymous writes are disabled"));

    let migrate_err = call_tool_error(&client, "admin_v2_migrate_metadata", json!({"dry_run": true})).await;
    assert!(migrate_err.contains("anonymous writes are disabled"));
}

#[tokio::test]
async fn v2_anonymous_deny_all_blocks_reads() {
    let client = setup_noop_server_with_auth(None, AnonymousPolicy::DenyAll).await;

    let recall_err = call_tool_error(
        &client,
        "recall",
        json!({
            "query": "anything"
        }),
    )
    .await;
    assert!(recall_err.contains("anonymous reads are disabled"));

    let scope_list_err = call_tool_error(&client, "admin_scope_list", json!({})).await;
    assert!(scope_list_err.contains("anonymous reads are disabled"));
}

#[tokio::test]
async fn v2_migration_report_counts_legacy_rows_missing_metadata() {
    let (client, _ids) = setup_noop_server_with_legacy_memories(vec![
        LegacySeed::new("legacy row without v2 metadata"),
        LegacySeed::new(format!(
            "{}\n```rust\nfn migration_report_code_candidate() {{}}\n```",
            "oversized migration report candidate ".repeat(130)
        )),
        LegacySeed::new("legacy duplicate migration report candidate"),
        LegacySeed::new("legacy duplicate migration report candidate"),
    ])
    .await;
    let _v2: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "v2 row with metadata summary",
            "summary": "v2 metadata summary",
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;

    let report: AdminV2MigrationReportResponse = call_tool(&client, "admin_v2_migration_report", json!({})).await;
    assert_eq!(report.report.total_memories, 5);
    assert_eq!(report.report.metadata_rows, 1);
    assert_eq!(report.report.missing_metadata, 4);
    assert_eq!(report.report.missing_summary, 4);
    assert_eq!(report.report.unresolved_scope, 4);
    assert_eq!(report.report.duplicate_candidates, 1);
    assert_eq!(report.report.oversized, 1);
    assert_eq!(report.report.code_derived, 1);
}

#[tokio::test]
async fn v2_metadata_migration_backfills_legacy_rows_non_destructively() {
    let (client, legacy_ids) = setup_noop_server_with_legacy_memories(vec![
        LegacySeed::new("legacy scoped durable fact for v2 metadata migration")
            .tags(&["migration-scoped"])
            .source_agent("legacy-agent")
            .source_conversation("gearboxlogic/localhold"),
        LegacySeed::new("legacy unregistered scope durable fact for v2 metadata migration")
            .tags(&["migration-unresolved"])
            .source_agent("legacy-agent")
            .source_conversation("old/unregistered"),
    ])
    .await;
    let scoped_id = legacy_ids[0];
    let unresolved_id = legacy_ids[1];

    register_localhold_scope(&client).await;

    let dry_run: AdminV2MigrateMetadataResponse = call_tool(&client, "admin_v2_migrate_metadata", json!({"dry_run": true})).await;
    assert!(dry_run.dry_run);
    assert_eq!(dry_run.report.candidate_count, 2);
    assert_eq!(dry_run.report.migrated, 0);
    assert_eq!(dry_run.report.unresolved_scope, 1);
    assert_eq!(dry_run.report.missing_summary, 2);

    let applied: AdminV2MigrateMetadataResponse = call_tool(&client, "admin_v2_migrate_metadata", json!({})).await;
    assert!(!applied.dry_run);
    assert_eq!(applied.report.candidate_count, 2);
    assert_eq!(applied.report.migrated, 2);
    assert_eq!(applied.report.unresolved_scope, 1);

    let after: AdminV2MigrationReportResponse = call_tool(&client, "admin_v2_migration_report", json!({})).await;
    assert_eq!(after.report.metadata_rows, 2);
    assert_eq!(after.report.missing_metadata, 0);
    assert_eq!(after.report.missing_summary, 2);
    assert_eq!(after.report.unresolved_scope, 1);

    let scoped_recall: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "legacy scoped durable fact",
            "tags": ["migration-scoped"],
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;
    assert_eq!(scoped_recall.count, 1);
    assert_eq!(scoped_recall.results[0].id, scoped_id);
    assert_eq!(scoped_recall.results[0].scope, "gearboxlogic/localhold");
    assert_eq!(scoped_recall.results[0].agent_label.as_deref(), Some("legacy-agent"));

    let unresolved_recall: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "legacy unregistered scope durable fact",
            "tags": ["migration-unresolved"]
        }),
    )
    .await;
    assert_eq!(unresolved_recall.count, 1);
    assert_eq!(unresolved_recall.results[0].id, unresolved_id);
    assert_eq!(unresolved_recall.results[0].scope, "inbox/unresolved");

    let read: ReadResponse = call_tool(&client, "read", json!({"id": scoped_id})).await;
    assert_eq!(read.memory.content, "legacy scoped durable fact for v2 metadata migration");
}

#[tokio::test]
async fn v2_metadata_migration_is_idempotent_and_preserves_existing_metadata() {
    let (client, legacy_ids) = setup_noop_server_with_legacy_memories(vec![
        LegacySeed::new("legacy idempotent migration row keeps original content")
            .tags(&["migration-idempotent"])
            .source_agent("legacy-agent")
            .source_conversation("gearboxlogic/localhold"),
    ])
    .await;
    let legacy_id = legacy_ids[0];

    register_localhold_scope(&client).await;

    let existing_v2: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": "existing v2 metadata should survive migration",
            "summary": "Existing v2 summary",
            "scope": "gearboxlogic/localhold",
            "agent_label": "Existing V2 Agent",
            "tags": ["migration-existing"]
        }),
    )
    .await;
    let first: AdminV2MigrateMetadataResponse = call_tool(&client, "admin_v2_migrate_metadata", json!({})).await;
    assert_eq!(first.report.candidate_count, 1);
    assert_eq!(first.report.skipped_existing, 1);
    assert_eq!(first.report.migrated, 1);

    let second: AdminV2MigrateMetadataResponse = call_tool(&client, "admin_v2_migrate_metadata", json!({})).await;
    assert_eq!(second.report.candidate_count, 0);
    assert_eq!(second.report.skipped_existing, 2);
    assert_eq!(second.report.migrated, 0);

    let report: AdminV2MigrationReportResponse = call_tool(&client, "admin_v2_migration_report", json!({})).await;
    assert_eq!(report.report.metadata_rows, 2);
    assert_eq!(report.report.missing_metadata, 0);
    assert_eq!(report.report.missing_summary, 1);

    let existing_recall: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "existing v2 metadata should survive migration",
            "tags": ["migration-existing"],
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;
    assert_eq!(existing_recall.results[0].id, existing_v2.id);
    assert_eq!(existing_recall.results[0].summary_or_excerpt, "Existing v2 summary");
    assert_eq!(existing_recall.results[0].agent_label.as_deref(), Some("Existing V2 Agent"));

    let legacy_recall: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "legacy idempotent migration row",
            "tags": ["migration-idempotent"],
            "scope": "gearboxlogic/localhold"
        }),
    )
    .await;
    assert_eq!(legacy_recall.results[0].id, legacy_id);
    assert_eq!(legacy_recall.results[0].agent_label.as_deref(), Some("legacy-agent"));

    let read: ReadResponse = call_tool(&client, "read", json!({"id": legacy_id})).await;
    assert_eq!(read.memory.content, "legacy idempotent migration row keeps original content");
}

#[tokio::test]
async fn v2_handoff_previews_without_commit() {
    let client = setup_noop_server().await;

    let handoff: HandoffResponse = call_tool(
        &client,
        "handoff",
        json!({
            "candidates": [
                {"content": "handoff candidate should be previewed only", "scope": "gearboxlogic/localhold", "tags": ["wip"]}
            ]
        }),
    )
    .await;

    assert!(!handoff.committed);
    assert_eq!(handoff.suggested_writes.len(), 1_usize);
    assert_eq!(handoff.suggested_writes[0].scope, "gearboxlogic/localhold");
    assert!(handoff.suggested_writes[0].id.is_none());

    let brief: BriefResponse = call_tool(&client, "brief", json!({"scope": "gearboxlogic/localhold"})).await;
    assert!(brief.relevant.is_empty(), "preview-only handoff must not persist candidates");
}

#[tokio::test]
async fn v2_handoff_rejects_oversized_batch_before_candidate_work() {
    let mut limits = LimitsConfig::default();
    limits.max_batch_size = 1;
    let (client, _server) = setup_noop_server_with_limits(limits).await;

    let err = call_tool_error(
        &client,
        "handoff",
        json!({
            "commit": true,
            "candidates": [
                {"content": "valid but oversized", "context_hints": [""]},
                {"content": "second handoff item"}
            ]
        }),
    )
    .await;
    let structured = parse_tool_error(&err);
    assert_eq!(structured.error.code, ToolErrorCode::InvalidParams);
    assert_eq!(structured.error.field.as_deref(), Some("candidates"));
    assert!(structured.error.message.contains("exceeds maximum batch size of 1"));
    assert!(!err.contains("context_hints"), "handoff cap should fail before per-candidate validation: {err}");

    let count: CountResponse = call_tool(&client, "admin_count", json!({})).await;
    assert_eq!(count.total, 0, "rejected committed handoff must not write any candidates");
}

#[tokio::test]
async fn v2_handoff_commit_validates_all_candidates_before_writing() {
    let client = setup_noop_server().await;

    assert_invalid_params_contains(
        &client,
        "handoff",
        json!({
            "commit": true,
            "candidates": [
                {"content": "valid handoff candidate must not be partially written"},
                {"content": "   "}
            ]
        }),
        "blank",
    )
    .await;

    let count: CountResponse = call_tool(&client, "admin_count", json!({})).await;
    assert_eq!(count.total, 0, "invalid committed handoff must not partially write earlier candidates");
}

#[tokio::test]
async fn v2_handoff_commit_batch_store_failure_does_not_write_candidates() {
    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_fail_batch_and_store_call(inner, 2_usize);
    let engine = RecallEngine::new(store, Arc::new(NoopEmbedding::new()), LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine(engine);
    let (server_transport, client_transport) = tokio::io::duplex(4096);

    let _server_task = tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });

    let client = ().serve(client_transport).await.unwrap();
    let args = json!({
        "commit": true,
        "candidates": [
            {"content": "first handoff candidate must roll back on later store failure"},
            {"content": "second handoff candidate triggers injected store failure"}
        ]
    });
    let result = client.call_tool(call_tool_params("handoff", args)).await;
    assert!(result.is_err(), "expected injected handoff store failure");
    let err = result.err().unwrap();
    assert!(err.to_string().contains("chaos"), "expected chaos store failure, got: {err}");

    let count: CountResponse = call_tool(&client, "admin_count", json!({})).await;
    assert_eq!(count.total, 0, "failed committed handoff must not partially persist earlier candidates");
}

#[tokio::test]
async fn v2_handoff_commit_uses_one_batch_store_call() {
    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_fail_on_store_call(inner, 2_usize);
    let engine = RecallEngine::new(store, Arc::new(NoopEmbedding::new()), LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine(engine);
    let (server_transport, client_transport) = tokio::io::duplex(4096);

    let _server_task = tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });

    let client = ().serve(client_transport).await.unwrap();
    let handoff: HandoffResponse = call_tool(
        &client,
        "handoff",
        json!({
            "commit": true,
            "candidates": [
                {"content": "first handoff candidate should be stored by the batch path"},
                {"content": "second handoff candidate would fail under per-item stores"}
            ]
        }),
    )
    .await;

    assert!(handoff.committed);
    assert_eq!(handoff.operation.changed, 2_u64);
    assert!(handoff.suggested_writes.iter().all(|write| write.id.is_some()));
}

#[tokio::test]
async fn v2_handoff_rejects_empty_batch() {
    let client = setup_noop_server().await;

    let err = call_tool_error(&client, "handoff", json!({"candidates": []})).await;
    let structured = parse_tool_error(&err);
    assert_eq!(structured.error.code, ToolErrorCode::InvalidParams);
    assert_eq!(structured.error.field.as_deref(), Some("candidates"));
    assert!(structured.error.message.contains("cannot be empty"));
}

#[tokio::test]
async fn v2_handoff_accepts_string_shorthand_candidates() {
    let client = setup_noop_server().await;

    let handoff: HandoffResponse = call_tool(
        &client,
        "handoff",
        json!({
            "candidates": [
                "handoff shorthand candidate should preview as content"
            ]
        }),
    )
    .await;

    assert!(!handoff.committed);
    assert_eq!(handoff.operation.status, OperationStatus::Preview);
    assert_eq!(handoff.suggested_writes.len(), 1_usize);
    assert_eq!(handoff.suggested_writes[0].content, "handoff shorthand candidate should preview as content");
    assert_eq!(handoff.suggested_writes[0].scope, "inbox/unresolved");
    assert!(handoff.suggested_writes[0].unresolved_scope);
    assert!(handoff.suggested_writes[0].id.is_none());
}

#[tokio::test]
async fn v2_handoff_warns_when_duplicate_candidates_exist() {
    let client = setup_noop_server().await;
    let content = "handoff duplicate warning should surface similar existing memories";

    let _existing: RememberResponse = call_tool(
        &client,
        "remember",
        json!({
            "content": content,
            "summary": "Handoff duplicate baseline",
            "scope": "gearboxlogic/localhold",
            "tags": ["wip"]
        }),
    )
    .await;

    let handoff: HandoffResponse = call_tool(
        &client,
        "handoff",
        json!({
            "candidates": [
                {
                    "content": content,
                    "summary": "Handoff duplicate candidate",
                    "scope": "gearboxlogic/localhold",
                    "tags": ["wip"]
                }
            ]
        }),
    )
    .await;

    assert_eq!(handoff.suggested_writes.len(), 1_usize);
    assert!(handoff.suggested_writes[0].warnings.iter().any(|warning| warning.code == "duplicate_candidate"));
}

#[tokio::test]
async fn v2_handoff_quality_warnings_are_advisory() {
    let client = setup_noop_server().await;
    let content = format!("{}\n```rust\nfn handoff_code_dump_candidate() {{}}\n```", "oversized handoff candidate ".repeat(170));

    let handoff: HandoffResponse = call_tool(
        &client,
        "handoff",
        json!({
            "commit": true,
            "candidates": [{
                "content": content
            }]
        }),
    )
    .await;

    assert!(handoff.committed);
    assert_eq!(handoff.suggested_writes.len(), 1_usize);
    assert_eq!(handoff.suggested_writes[0].scope, "inbox/unresolved");
    assert!(handoff.suggested_writes[0].unresolved_scope);
    assert!(handoff.suggested_writes[0].id.is_some(), "committed handoff should still persist warned writes");
    assert_warning_codes(&handoff.suggested_writes[0].warnings, &[
        "missing_scope",
        "missing_summary",
        "empty_tags",
        "empty_entities",
        "oversized_content",
        "possible_code_dump",
    ]);

    let read: ReadResponse = call_tool(&client, "read", json!({ "id": handoff.suggested_writes[0].id.unwrap() })).await;
    assert_eq!(read.memory.content, content);
}

#[tokio::test]
async fn v2_handoff_resolves_scope_from_context_hints_without_commit() {
    let client = setup_noop_server().await;

    let _registered: serde_json::Value = call_tool(
        &client,
        "admin_scope_register",
        json!({
            "scope_key": "gearboxlogic/localhold",
            "display_name": "LocalHold",
            "matchers": ["/workspace/localhold"]
        }),
    )
    .await;

    let handoff: HandoffResponse = call_tool(
        &client,
        "handoff",
        json!({
            "candidates": [
                {
                    "content": "handoff candidate should use context hints for scope",
                    "context_hints": ["/workspace/localhold/src/server/mod.rs"],
                    "tags": ["wip"]
                }
            ]
        }),
    )
    .await;

    assert!(!handoff.committed);
    assert_eq!(handoff.suggested_writes.len(), 1_usize);
    assert_eq!(handoff.suggested_writes[0].scope, "gearboxlogic/localhold");
    assert!(!handoff.suggested_writes[0].unresolved_scope);
    assert_eq!(handoff.suggested_writes[0].scope_resolution.resolved_by, ScopeResolvedBy::Matcher);
    assert!(handoff.suggested_writes[0].warnings.iter().all(|warning| warning.code != "missing_scope"));
    assert!(handoff.suggested_writes[0].id.is_none());
}

#[tokio::test]
async fn server_info_has_capabilities() {
    let client = setup_noop_server().await;
    #[expect(clippy::expect_used, reason = "test assertion: peer_info must exist after handshake")]
    let info = client.peer_info().expect("should have peer info after handshake");
    assert!(info.capabilities.tools.is_some(), "server should advertise tools capability");
    let instructions = info.instructions.as_deref().unwrap_or_default();
    assert!(instructions.contains("brief"), "server instructions should guide agents to the v2 core workflow");
    assert!(
        instructions.contains("Legacy v1 memory_* names are not part of the public MCP tool surface"),
        "server instructions should not imply legacy tools are available"
    );
}

#[tokio::test]
async fn call_nonexistent_tool_returns_error() {
    let client = setup_noop_server().await;
    let result = client.call_tool(call_tool_params("nonexistent_tool", json!({}))).await;

    // Should be either a protocol-level error or an application-level error — not a panic
    assert!(result.is_err() || result.unwrap().is_error.unwrap_or(false));
}
