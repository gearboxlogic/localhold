use std::sync::Arc;

use chrono::Utc;
use localhold::{
    config::{AnonymousPolicy, LimitsConfig, SearchConfig},
    embedding::NoopEmbedding,
    engine::RecallEngine,
    server::{
        RecallServer,
        params::{AdminListResponse, HistoryResponse, ReadManyResponse, ReadResponse, RecallResponse},
    },
    store::{MemoryAdmin as _, MemoryWriter as _, SqliteStore},
    types::{AccessPolicy, AuditAction, Memory, MemoryId, Provenance, RedactableField, V2MemoryMetadata},
};
use rmcp::{ServiceExt as _, service::RunningService};
use serde_json::json;

use super::helpers::{call_tool, call_tool_error};

#[derive(Clone)]
struct Seed {
    content: &'static str,
    tags: Vec<&'static str>,
    owner: Option<&'static str>,
    scope: Option<&'static str>,
    policy: AccessPolicy,
    metadata: Option<SeedMetadata>,
}

#[derive(Clone)]
struct SeedMetadata {
    summary: Option<&'static str>,
    scope: Option<&'static str>,
    agent_label: Option<&'static str>,
    created_by: Option<&'static str>,
    quality_flags: Vec<&'static str>,
}

impl SeedMetadata {
    const fn new() -> Self {
        Self {
            summary: None,
            scope: None,
            agent_label: None,
            created_by: None,
            quality_flags: Vec::new(),
        }
    }

    const fn summary(mut self, summary: &'static str) -> Self {
        self.summary = Some(summary);
        self
    }

    const fn scope(mut self, scope: &'static str) -> Self {
        self.scope = Some(scope);
        self
    }

    const fn agent_label(mut self, agent_label: &'static str) -> Self {
        self.agent_label = Some(agent_label);
        self
    }

    const fn created_by(mut self, created_by: &'static str) -> Self {
        self.created_by = Some(created_by);
        self
    }

    fn quality_flags(mut self, flags: Vec<&'static str>) -> Self {
        self.quality_flags = flags;
        self
    }
}

impl Seed {
    const fn public(content: &'static str) -> Self {
        Self {
            content,
            tags: Vec::new(),
            owner: Some("owner"),
            scope: None,
            policy: AccessPolicy::Public,
            metadata: None,
        }
    }

    fn restricted(content: &'static str) -> Self {
        Self {
            content,
            tags: Vec::new(),
            owner: Some("owner"),
            scope: None,
            policy: AccessPolicy::Restricted {
                allowed: vec!["friend".to_owned()],
            },
            metadata: None,
        }
    }

    const fn redacted(content: &'static str, visible_fields: Vec<RedactableField>) -> Self {
        Self {
            content,
            tags: Vec::new(),
            owner: Some("owner"),
            scope: None,
            policy: AccessPolicy::Redacted { visible_fields },
            metadata: None,
        }
    }

    fn tags(mut self, tags: Vec<&'static str>) -> Self {
        self.tags = tags;
        self
    }

    const fn scope(mut self, scope: &'static str) -> Self {
        self.scope = Some(scope);
        self
    }

    fn metadata(mut self, metadata: SeedMetadata) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

async fn setup_seeded_server(principal: Option<&str>, seeds: Vec<Seed>) -> (RunningService<rmcp::RoleClient, ()>, Vec<MemoryId>) {
    let store = SqliteStore::in_memory().unwrap();
    let mut ids = Vec::with_capacity(seeds.len());
    for seed in seeds {
        let provenance = Provenance::new_for_test(seed.owner.map(ToOwned::to_owned), seed.scope.map(ToOwned::to_owned), None);
        let memory = Memory::new_for_test(seed.content.to_owned(), seed.tags.into_iter().map(ToOwned::to_owned).collect(), provenance, seed.policy);
        let id = store.store(&memory, None).await.unwrap();
        if let Some(metadata) = seed.metadata {
            let metadata = serde_json::from_value::<V2MemoryMetadata>(json!({
                "memory_id": id,
                "scope_key": metadata.scope,
                "summary": metadata.summary,
                "agent_label": metadata.agent_label,
                "created_by_principal": metadata.created_by,
                "quality_flags": metadata.quality_flags,
                "schema_version": 2_i32,
            }))
            .unwrap();
            store.upsert_v2_metadata(metadata).await.unwrap();
        }
        ids.push(id);
    }

    let client = serve_store(principal, store).await;
    (client, ids)
}

async fn serve_store(principal: Option<&str>, store: SqliteStore) -> RunningService<rmcp::RoleClient, ()> {
    let engine = RecallEngine::new(store, Arc::new(NoopEmbedding::new()), LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine_with_auth(engine, principal.map(ToOwned::to_owned), AnonymousPolicy::PublicReadOnly);

    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let _server_task = tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });
    ().serve(client_transport).await.unwrap()
}

#[tokio::test]
async fn redacted_policy_returns_full_content_for_owner_and_redacted_view_for_other_principal() {
    let (owner_client, owner_ids) = setup_seeded_server(Some("owner"), vec![Seed::redacted("redacted secret", vec![RedactableField::Content]).tags(vec!["secret"])]).await;
    let owner_read: ReadResponse = call_tool(&owner_client, "read", json!({"id": owner_ids[0]})).await;
    assert_eq!(owner_read.memory.content, "redacted secret");
    assert_eq!(owner_read.memory.tags, vec!["secret"]);
    assert_eq!(owner_read.agent_label.as_deref(), Some("owner"));

    let (other_client, other_ids) = setup_seeded_server(Some("other"), vec![Seed::redacted("redacted secret", vec![RedactableField::Content]).tags(vec!["secret"])]).await;
    let other_read: ReadResponse = call_tool(&other_client, "read", json!({"id": other_ids[0]})).await;
    assert_eq!(other_read.memory.content, "redacted secret");
    assert!(other_read.memory.tags.is_empty(), "tags should be hidden for non-owner redacted reads");

    let (anonymous_client, anonymous_ids) = setup_seeded_server(None, vec![Seed::redacted("redacted secret", vec![RedactableField::Content])]).await;
    let err = call_tool_error(&anonymous_client, "read", json!({"id": anonymous_ids[0]})).await;
    assert!(err.contains("not found"), "expected anonymous redacted read to be denied, got: {err}");
}

#[tokio::test]
async fn restricted_policy_allows_owner_and_allowed_principal_only() {
    let (friend_client, friend_ids) = setup_seeded_server(Some("friend"), vec![Seed::restricted("restricted data")]).await;
    let friend_read: ReadResponse = call_tool(&friend_client, "read", json!({"id": friend_ids[0]})).await;
    assert_eq!(friend_read.memory.content, "restricted data");

    let (owner_client, owner_ids) = setup_seeded_server(Some("owner"), vec![Seed::restricted("restricted data")]).await;
    let owner_read: ReadResponse = call_tool(&owner_client, "read", json!({"id": owner_ids[0]})).await;
    assert_eq!(owner_read.memory.content, "restricted data");

    let (other_client, other_ids) = setup_seeded_server(Some("other"), vec![Seed::restricted("restricted data")]).await;
    let err = call_tool_error(&other_client, "read", json!({"id": other_ids[0]})).await;
    assert!(err.contains("not found"), "expected other principal to be denied, got: {err}");

    let (anonymous_client, anonymous_ids) = setup_seeded_server(None, vec![Seed::restricted("restricted data")]).await;
    let err = call_tool_error(&anonymous_client, "read", json!({"id": anonymous_ids[0]})).await;
    assert!(err.contains("not found"), "expected anonymous restricted read to be denied, got: {err}");
}

#[tokio::test]
async fn access_policy_filters_admin_list() {
    let seeds = vec![
        Seed::public("public info"),
        Seed::restricted("restricted info"),
        Seed::redacted("redacted info", Vec::new()),
    ];

    let (anonymous_client, _) = setup_seeded_server(None, seeds.clone()).await;
    let anonymous: AdminListResponse = call_tool(&anonymous_client, "admin_list", json!({})).await;
    assert_eq!(anonymous.count, 1);
    assert_eq!(anonymous.memories[0].summary_or_excerpt, "public info");

    let (friend_client, _) = setup_seeded_server(Some("friend"), seeds.clone()).await;
    let friend: AdminListResponse = call_tool(&friend_client, "admin_list", json!({})).await;
    assert_eq!(friend.count, 3);
    assert!(
        friend.memories.iter().any(|memory| memory.summary_or_excerpt == "[redacted]"),
        "redacted memory should be visible as a redacted inventory card"
    );

    let (owner_client, _) = setup_seeded_server(Some("owner"), seeds).await;
    let owner: AdminListResponse = call_tool(&owner_client, "admin_list", json!({})).await;
    assert_eq!(owner.count, 3);
    assert!(owner.memories.iter().any(|memory| memory.summary_or_excerpt == "redacted info"));
}

#[tokio::test]
async fn access_policy_filters_recall_cards() {
    let seeds = vec![Seed::public("database optimization public"), Seed::restricted("database secrets restricted")];

    let (anonymous_client, _) = setup_seeded_server(None, seeds.clone()).await;
    let anonymous: RecallResponse = call_tool(&anonymous_client, "recall", json!({"query": "database", "include_weak": true})).await;
    assert_eq!(anonymous.count, 1);
    assert!(anonymous.results[0].summary_or_excerpt.contains("public"));

    let (friend_client, _) = setup_seeded_server(Some("friend"), seeds).await;
    let friend: RecallResponse = call_tool(&friend_client, "recall", json!({"query": "database", "include_weak": true})).await;
    assert_eq!(friend.count, 2);
    assert!(friend.results.iter().any(|memory| memory.summary_or_excerpt.contains("restricted")));
}

#[tokio::test]
async fn redacted_visible_fields_hide_tags_and_provenance_for_non_owner() {
    let (client, ids) = setup_seeded_server(Some("other"), vec![
        Seed::redacted("partially visible", vec![RedactableField::Content]).tags(vec!["important", "secret"]),
    ])
    .await;

    let read: ReadResponse = call_tool(&client, "read", json!({"id": ids[0]})).await;
    assert_eq!(read.memory.content, "partially visible");
    assert!(read.memory.tags.is_empty(), "tags should be redacted for non-owner");
}

#[tokio::test]
async fn redacted_v2_metadata_does_not_restore_hidden_read_fields() {
    let metadata = SeedMetadata::new()
        .summary("hidden summary")
        .scope("secret/scope")
        .agent_label("metadata-agent")
        .created_by("owner")
        .quality_flags(vec!["missing_scope", "oversized_content"]);
    let seed = Seed::redacted("classified launch code", Vec::new())
        .tags(vec!["secret"])
        .scope("secret/scope")
        .metadata(metadata);
    let (client, ids) = setup_seeded_server(Some("other"), vec![seed]).await;

    let read: ReadResponse = call_tool(&client, "read", json!({"id": ids[0]})).await;
    assert_eq!(read.memory.content, "[redacted]");
    assert!(read.memory.tags.is_empty());
    assert_eq!(read.summary, None);
    assert_eq!(read.scope, None);
    assert_eq!(read.agent_label, None);
    assert_eq!(read.created_by_principal, None);
    assert!(read.quality_flags.is_empty());
    assert!(!read.unresolved_scope);

    let read_many: ReadManyResponse = call_tool(&client, "read_many", json!({"ids": [ids[0]]})).await;
    let item = &read_many.results[0];
    assert_eq!(item.memory.as_ref().unwrap().content, "[redacted]");
    assert_eq!(item.summary, None);
    assert_eq!(item.scope, None);
    assert_eq!(item.agent_label, None);
    assert_eq!(item.created_by_principal, None);
    assert!(item.quality_flags.is_empty());

    let listed: AdminListResponse = call_tool(&client, "admin_list", json!({})).await;
    assert_eq!(listed.count, 1);
    let card = &listed.memories[0];
    assert_eq!(card.summary_or_excerpt, "[redacted]");
    assert_eq!(card.scope, "[redacted]");
    assert_eq!(card.agent_label, None);
    assert!(card.quality_flags.is_empty());
    assert!(!card.unresolved_scope);
}

#[tokio::test]
async fn redacted_recall_card_hides_diagnostics_and_hidden_metadata() {
    let metadata = SeedMetadata::new()
        .summary("visible summary")
        .scope("secret/scope")
        .agent_label("metadata-agent")
        .created_by("owner")
        .quality_flags(vec!["missing_scope"]);
    let seed = Seed::redacted("visible codename redactionprobe", vec![RedactableField::Content])
        .scope("secret/scope")
        .metadata(metadata);
    let (client, ids) = setup_seeded_server(Some("other"), vec![seed]).await;

    let recall: RecallResponse = call_tool(&client, "recall", json!({"query": "redactionprobe", "include_weak": true})).await;

    assert_eq!(recall.count, 1);
    assert_eq!(recall.results[0].id, ids[0]);
    assert_eq!(recall.results[0].summary_or_excerpt, "visible summary");
    assert_eq!(recall.results[0].scope, "[redacted]");
    assert_eq!(recall.results[0].agent_label, None);
    assert_eq!(recall.results[0].diagnostics.retrieval_score, None);
    assert_eq!(recall.results[0].diagnostics.reranker_score, None);
    assert_eq!(recall.results[0].diagnostics.reranker_blend_weight, None);
    assert_eq!(recall.results[0].diagnostics.vector_distance, None);
    assert_eq!(recall.results[0].diagnostics.ranking_score, None);
}

#[tokio::test]
async fn hidden_content_redacted_memory_is_not_discoverable_by_search() {
    let seeds = vec![
        Seed::redacted("hidden redactionprobe", Vec::new()),
        Seed::redacted("visible redactionprobe", vec![RedactableField::Content]),
    ];
    let (client, ids) = setup_seeded_server(Some("other"), seeds).await;

    let recall: RecallResponse = call_tool(&client, "recall", json!({"query": "redactionprobe", "include_weak": true})).await;
    assert_eq!(recall.results.iter().map(|card| card.id).collect::<Vec<_>>(), vec![ids[1]]);

    let listed: AdminListResponse = call_tool(&client, "admin_list", json!({"text_search": "redactionprobe"})).await;
    assert_eq!(listed.memories.iter().map(|card| card.id).collect::<Vec<_>>(), vec![ids[1]]);

    let hidden_read: ReadResponse = call_tool(&client, "read", json!({"id": ids[0]})).await;
    assert_eq!(hidden_read.memory.content, "[redacted]", "direct reads still return the redacted view");
}

#[tokio::test]
async fn redacted_hidden_filter_fields_do_not_disclose_memory_presence() {
    let seeds = vec![
        Seed::redacted("visible but hidden metadata", vec![RedactableField::Content])
            .tags(vec!["secret-tag"])
            .scope("secret/scope"),
        Seed::redacted("visible with tag", vec![RedactableField::Content, RedactableField::Tags]).tags(vec!["visible-tag"]),
    ];
    let (client, ids) = setup_seeded_server(Some("other"), seeds).await;

    let unfiltered: AdminListResponse = call_tool(&client, "admin_list", json!({})).await;
    assert_eq!(unfiltered.count, 2, "redacted memories remain visible without hidden-field filters");

    let hidden_tag: AdminListResponse = call_tool(&client, "admin_list", json!({"tags": ["secret-tag"]})).await;
    assert_eq!(hidden_tag.count, 0, "hidden tags must not be filter-discoverable");

    let hidden_scope: AdminListResponse = call_tool(&client, "admin_list", json!({"scope": "secret/scope", "expand_scopes": false})).await;
    assert_eq!(hidden_scope.count, 0, "hidden provenance scope must not be filter-discoverable");

    let visible_tag: AdminListResponse = call_tool(&client, "admin_list", json!({"tags": ["visible-tag"]})).await;
    assert_eq!(visible_tag.memories.iter().map(|card| card.id).collect::<Vec<_>>(), vec![ids[1]]);
}

#[tokio::test]
async fn admin_history_redacted_view_omits_principal_and_details() {
    let store = SqliteStore::in_memory().unwrap();
    let mut memory = Memory::new_for_test(
        "visible audited content".to_owned(),
        Vec::new(),
        Provenance::new_for_test(Some("owner".to_owned()), Some("secret/scope".to_owned()), None),
        AccessPolicy::Redacted {
            visible_fields: vec![RedactableField::Content],
        },
    );
    memory.updated_at = memory.created_at;
    let id = store.store(&memory, None).await.unwrap();
    store
        .write_audit_entry(
            &id,
            AuditAction::Update,
            Some("owner"),
            Utc::now(),
            Some(&json!({"old_content_hash": "hidden-hash", "scope": "secret/scope"})),
        )
        .await
        .unwrap();

    let engine = RecallEngine::new(store, Arc::new(NoopEmbedding::new()), LimitsConfig::default(), SearchConfig::default());
    let server = RecallServer::from_engine_with_auth(engine, Some("other".to_owned()), AnonymousPolicy::PublicReadOnly);
    let (server_transport, client_transport) = tokio::io::duplex(4096);
    let _server_task = tokio::spawn(async move {
        let svc = server.serve(server_transport).await.unwrap();
        let _completed = svc.waiting().await;
    });
    let client = ().serve(client_transport).await.unwrap();

    let history: HistoryResponse = call_tool(&client, "admin_history", json!({"id": id})).await;

    assert_eq!(history.entries.len(), 1);
    assert_eq!(history.entries[0].action, AuditAction::Update);
    assert_eq!(history.entries[0].principal, None);
    assert_eq!(history.entries[0].details, None);
}

#[tokio::test]
async fn deleted_memory_history_uses_tombstone_authorization() {
    let store = SqliteStore::in_memory().unwrap();
    let memory = Memory::new_for_test(
        "restricted deleted content".to_owned(),
        Vec::new(),
        Provenance::new_for_test(Some("owner".to_owned()), Some("deleted/scope".to_owned()), None),
        AccessPolicy::Restricted {
            allowed: vec!["friend".to_owned()],
        },
    );
    let id = store.store(&memory, None).await.unwrap();

    let owner = serve_store(Some("owner"), store.clone()).await;
    let _deleted: serde_json::Value = call_tool(&owner, "forget", json!({"id": id})).await;

    let friend = serve_store(Some("friend"), store.clone()).await;
    let friend_history: HistoryResponse = call_tool(&friend, "admin_history", json!({"id": id})).await;
    assert_eq!(friend_history.entries.len(), 1);
    assert_eq!(friend_history.entries[0].action, AuditAction::Delete);
    assert_eq!(friend_history.entries[0].principal.as_deref(), Some("owner"));

    let intruder = serve_store(Some("intruder"), store).await;
    let intruder_history: HistoryResponse = call_tool(&intruder, "admin_history", json!({"id": id})).await;
    assert!(intruder_history.entries.is_empty());
}

#[tokio::test]
async fn deleted_redacted_memory_history_omits_principal_and_details_for_redacted_tombstone_view() {
    let store = SqliteStore::in_memory().unwrap();
    let memory = Memory::new_for_test(
        "redacted deleted content".to_owned(),
        Vec::new(),
        Provenance::new_for_test(Some("owner".to_owned()), Some("deleted/scope".to_owned()), None),
        AccessPolicy::Redacted {
            visible_fields: vec![RedactableField::Content],
        },
    );
    let id = store.store(&memory, None).await.unwrap();

    let owner = serve_store(Some("owner"), store.clone()).await;
    let _deleted: serde_json::Value = call_tool(&owner, "forget", json!({"id": id})).await;

    let other = serve_store(Some("other"), store).await;
    let history: HistoryResponse = call_tool(&other, "admin_history", json!({"id": id})).await;
    assert_eq!(history.entries.len(), 1);
    assert_eq!(history.entries[0].action, AuditAction::Delete);
    assert_eq!(history.entries[0].principal, None);
    assert_eq!(history.entries[0].details, None);
}

#[tokio::test]
async fn deleted_memory_history_without_tombstone_fails_closed() {
    let store = SqliteStore::in_memory().unwrap();
    let id = MemoryId::new();
    store.write_audit_entry(&id, AuditAction::Delete, Some("owner"), Utc::now(), None).await.unwrap();

    let owner = serve_store(Some("owner"), store).await;
    let history: HistoryResponse = call_tool(&owner, "admin_history", json!({"id": id})).await;

    assert!(history.entries.is_empty());
}
