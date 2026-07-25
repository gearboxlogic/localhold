//! Shared store contract checks for every `MemoryStore` backend.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, TimeZone as _, Utc};
use serde_json::json;

use super::{MemoryStore, MemoryWithEmbedding};
use crate::{
    context::{
        ContextAnchorPolicy, ContextAnchorPolicyDraft, ContextAuditDraft, ContextCreateDraft, ContextDefinitionPatch, ContextExactLookup, ContextId, ContextKind, ContextKindDraft,
        ContextKindPolicy, ContextKindPolicyDraft, ContextLifecycle, ContextPolicyLayer, OPERATOR_PRINCIPAL,
    },
    error::StoreError,
    types::{
        AccessPolicy, AuditAction, AuditDraft, Confidence, Entity, Importance, Memory, MemoryFilter, MemoryId, MemoryMetadata, MemoryType, MemoryUpdate, MetadataPatch, Provenance,
        QueryContext, RedactableField, ScopeDefinition, SearchResult, WriteOutcome,
    },
};

const OWNER: &str = "conformance-owner";
const ALLOWED: &str = "conformance-allowed";
const VIEWER: &str = "conformance-viewer";

struct MemorySpec {
    content: String,
    tags: Vec<String>,
    source_agent: &'static str,
    scope: String,
    origin: String,
    access_policy: AccessPolicy,
    created_at: DateTime<Utc>,
}

/// Exercise the backend-neutral store contract for remediated behavior.
#[expect(clippy::too_many_lines, reason = "single shared fixture intentionally exercises the full backend-neutral MemoryStore contract")]
pub(crate) async fn assert_memory_store_contract<S>(store: &S, embedding_dimensions: usize)
where
    S: MemoryStore,
{
    assert!(embedding_dimensions > 0, "conformance requires positive embedding dimensions");

    let case = MemoryId::new().to_string();
    let case_tag = format!("contract-{case}");
    let scope = format!("contract/scope/{case}");
    let origin = format!("contract/origin/{case}");
    let owner_ctx = QueryContext { principal: Some(OWNER.into()) };
    let viewer_ctx = QueryContext { principal: Some(VIEWER.into()) };
    let base = fixed_time();

    let scope_def = ScopeDefinition {
        scope_key: scope.clone(),
        display_name: format!("Contract {case}"),
        description: Some("store conformance fixture".into()),
        aliases: vec![format!("alias-{case}")],
        matchers: vec![case.clone()],
        parent: Some("contract".into()),
        related: vec![format!("contract/related/{case}")],
    };
    store.register_scope(scope_def.clone()).await.unwrap();
    assert!(store.list_scopes().await.unwrap().contains(&scope_def));
    let context_id = store
        .list_context_records(OWNER, false, 0, 500)
        .await
        .unwrap()
        .into_iter()
        .find(|record| record.context.key == scope)
        .unwrap()
        .context
        .id;

    let primary_token = format!("contractneedlealpha{case}");
    let entity_name = format!("ContractEntity{case}");
    let mut primary = memory(MemorySpec {
        content: format!("primary searchable memory {primary_token}"),
        tags: vec![case_tag.clone(), "primary".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Public,
        created_at: base,
    });
    primary.memory_type = MemoryType::Procedural;
    primary.entities = vec![Entity::new(entity_name.clone(), "project").unwrap()];
    let primary_embedding = embedding(embedding_dimensions, 0.0_f32);
    let primary_id = store.store(&primary, Some(&primary_embedding)).await.unwrap();
    assert_eq!(primary_id, primary.id);
    let context_audit = ContextAuditDraft {
        actor_principal: OWNER.into(),
        action: "conformance_membership".into(),
        context_id: None,
        memory_id: Some(primary_id),
        details: None,
    };
    assert_eq!(
        store.replace_memory_contexts(&primary_id, &[context_id], OWNER, &context_audit).await.unwrap(),
        WriteOutcome::Applied
    );

    let retrieved = store.get(&primary_id, None).await.unwrap().unwrap();
    assert_eq!(retrieved.content, primary.content);
    assert_eq!(retrieved.entities, primary.entities);
    let batched = store.get_batch(&[primary_id, MemoryId::new(), primary_id], None).await.unwrap();
    assert_eq!(batched.len(), 1);
    assert_eq!(batched[&primary_id].entities, primary.entities);
    let mut oversized_sql_parameter_batch = std::iter::repeat_with(MemoryId::new).take(1_100_usize).collect::<Vec<_>>();
    oversized_sql_parameter_batch.push(primary_id);
    let oversized_batch_result = store.get_batch(&oversized_sql_parameter_batch, None).await.unwrap();
    assert_eq!(oversized_batch_result.len(), 1, "batch reads must not consume one SQL bind parameter per requested ID");
    assert_eq!(oversized_batch_result[&primary_id].content, primary.content);
    let batched_contexts = store.get_memory_contexts_batch(&[primary_id], OWNER).await.unwrap();
    assert_eq!(batched_contexts[&primary_id][0].context.id, context_id);
    assert_eq!(store.count_memory_contexts_for_write(&primary_id, OWNER).await.unwrap(), Some(1));
    let mut blank_principal_memory = primary.clone();
    blank_principal_memory.id = MemoryId::new();
    blank_principal_memory.provenance.source_agent = Some(" \t".into());
    let blank_metadata = MemoryMetadata {
        memory_id: blank_principal_memory.id,
        scope_key: Some(scope.clone()),
        summary: None,
        agent_label: None,
        created_by_principal: None,
        quality_flags: Vec::new(),
        schema_version: 1,
    };
    let blank_audit = AuditDraft {
        action: AuditAction::Store,
        caller_agent: None,
        timestamp: blank_principal_memory.created_at,
        details: None,
    };
    let blank_context_audit = ContextAuditDraft {
        actor_principal: "anonymous".into(),
        action: "conformance_blank_principal_rejected".into(),
        context_id: None,
        memory_id: Some(blank_principal_memory.id),
        details: None,
    };
    let blank_error = store
        .store_with_metadata_contexts_audited(&blank_principal_memory, None, None, &blank_metadata, &[context_id], &blank_audit, &blank_context_audit)
        .await
        .unwrap_err();
    assert!(blank_error.to_string().contains("source principal"));
    assert!(store.get(&blank_principal_memory.id, None).await.unwrap().is_none());

    let filter = MemoryFilter {
        tags: Some(vec![case_tag.clone(), "primary".into()]),
        scope: Some(scope.clone()),
        origin_scope: Some(origin.clone()),
        text_search: Some(primary_token.clone()),
        has_embedding: Some(true),
        memory_type: Some(MemoryType::Procedural),
        entity: Some(entity_name.clone()),
        entity_type: Some("project".into()),
        limit: Some(10),
        ..MemoryFilter::default()
    };
    let listed = store.list(filter.clone(), owner_ctx.clone()).await.unwrap();
    assert_eq!(ids(&listed), vec![primary_id]);
    let stats = store.count(filter.clone(), owner_ctx.clone(), 10).await.unwrap();
    assert_eq!(stats.total, 1_u64);
    assert_eq!(stats.with_embedding, 1_u64);
    assert_eq!(stats.without_embedding, 0_u64);
    assert!(stats.by_tag.iter().any(|(tag, count)| tag == "primary" && *count == 1_u64));

    let text_results = store.search_by_text(&primary_token, 10, &case_filter(&case_tag), &owner_ctx).await.unwrap();
    assert_search_contains(&text_results, primary_id);
    if store.fts_available() {
        let fts_results = store.search_by_fts(&primary_token, 10, &case_filter(&case_tag), &owner_ctx, None).await.unwrap();
        assert_search_contains(&fts_results, primary_id);
    }

    let semantic_results = store.search_by_embedding(&primary_embedding, 10, &filter, &owner_ctx, Some(0.001_f64)).await.unwrap();
    assert_eq!(search_ids(&semantic_results), vec![primary_id]);
    let fetched_embeddings = store.fetch_embeddings_for_ids(&[primary_id]).await.unwrap();
    assert_eq!(fetched_embeddings.get(&primary_id).map(Vec::len), Some(embedding_dimensions));
    assert_eq!(fetched_embeddings.get(&primary_id).and_then(|v| v.first()).copied(), Some(0.0_f32));

    let scoped_embeddings = store.list_with_embeddings(Some(std::slice::from_ref(&context_id)), None, OWNER, 10).await.unwrap();
    assert!(scoped_embeddings.iter().any(|entry| entry.memory.id == primary_id && entry.embedding.is_some()));

    let restricted = memory(MemorySpec {
        content: format!("restricted memory {case}"),
        tags: vec![case_tag.clone(), "restricted".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Restricted { allowed: vec![ALLOWED.into()] },
        created_at: time_after(base, 1),
    });
    let restricted_id = store.store(&restricted, Some(&embedding(embedding_dimensions, 3.0_f32))).await.unwrap();
    assert!(store.get(&restricted_id, Some("intruder")).await.unwrap().is_none());
    assert!(store.get(&restricted_id, Some(ALLOWED)).await.unwrap().is_some());
    let denied_newer = memory(MemorySpec {
        content: format!("newer unauthorized consolidation candidate {case}"),
        tags: vec![case_tag.clone(), "denied-candidate".into()],
        source_agent: "different-owner",
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Restricted {
            allowed: vec!["different-principal".into()],
        },
        created_at: time_after(base, 2),
    });
    let denied_newer_id = store.store(&denied_newer, Some(&embedding(embedding_dimensions, 4.0_f32))).await.unwrap();
    assert_eq!(
        store
            .list_with_embeddings(None, None, ALLOWED, 1)
            .await
            .unwrap()
            .into_iter()
            .map(|candidate| candidate.memory.id)
            .collect::<Vec<_>>(),
        vec![restricted_id],
        "write authorization must be applied before the consolidation candidate limit"
    );
    assert!(
        store
            .list_with_embeddings(None, None, "intruder", 100)
            .await
            .unwrap()
            .iter()
            .all(|candidate| candidate.memory.id != restricted_id && candidate.memory.id != denied_newer_id)
    );
    assert!(
        store
            .list_with_embeddings(None, None, OWNER, 100)
            .await
            .unwrap()
            .iter()
            .any(|candidate| candidate.memory.id == restricted_id),
        "the internal whole-store candidate path includes contextless memories"
    );
    assert!(
        store
            .list_with_embeddings(Some(&[]), None, OWNER, 100)
            .await
            .unwrap()
            .iter()
            .all(|candidate| candidate.memory.id != restricted_id),
        "the governed broad consolidation path excludes contextless memories"
    );

    let hidden_token = format!("hiddencontractneedle{case}");
    let redacted = memory(MemorySpec {
        content: format!("redacted hidden content {hidden_token}"),
        tags: vec![case_tag.clone(), "redacted".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Redacted {
            visible_fields: vec![RedactableField::Tags],
        },
        created_at: time_after(base, 2),
    });
    let redacted_id = store.store(&redacted, Some(&embedding(embedding_dimensions, 5.0_f32))).await.unwrap();
    let redacted_view = store.get(&redacted_id, Some(VIEWER)).await.unwrap().unwrap();
    assert!(redacted_view.was_redacted);
    assert_eq!(redacted_view.content, "[redacted]");
    assert_eq!(redacted_view.tags, redacted.tags);
    let redacted_filter = MemoryFilter {
        tags: Some(vec![case_tag.clone(), "redacted".into()]),
        ..MemoryFilter::default()
    };
    let viewer_hidden_results = store.search_by_text(&hidden_token, 10, &redacted_filter, &viewer_ctx).await.unwrap();
    assert!(!search_ids(&viewer_hidden_results).contains(&redacted_id));
    let owner_hidden_results = store.search_by_text(&hidden_token, 10, &redacted_filter, &owner_ctx).await.unwrap();
    assert_search_contains(&owner_hidden_results, redacted_id);

    let old = memory(MemorySpec {
        content: format!("superseded old {case}"),
        tags: vec![case_tag.clone(), "supersession".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 3),
    });
    let old_id = store.store(&old, Some(&embedding(embedding_dimensions, 6.0_f32))).await.unwrap();
    let opened_old = store.get(&old_id, Some(OWNER)).await.unwrap().unwrap();
    let new = memory(MemorySpec {
        content: format!("superseded new {case}"),
        tags: vec![case_tag.clone(), "supersession".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 4),
    });
    let new_id = store.store_with_supersession(&new, Some(&embedding(embedding_dimensions, 6.1_f32)), &old_id).await.unwrap();
    let superseded = store.get(&old_id, Some(OWNER)).await.unwrap().unwrap();
    assert!(superseded.record_revision > opened_old.record_revision, "supersession must advance the optimistic revision");
    assert_eq!(superseded.updated_at, opened_old.updated_at, "supersession must preserve content freshness");
    let supersession_audit = AuditDraft {
        action: AuditAction::Update,
        caller_agent: Some(OWNER.into()),
        timestamp: time_after(base, 4),
        details: Some(json!({"stale_after_supersession": true})),
    };
    let stale_after_supersession = store
        .update_authorized_if_unmodified_with_metadata_audited(
            &old_id,
            opened_old.record_revision,
            &MemoryUpdate {
                importance: Some(Importance::new(0.6_f64)),
                ..MemoryUpdate::default()
            },
            None,
            None,
            OWNER,
            &supersession_audit,
        )
        .await
        .unwrap_err();
    assert!(matches!(stale_after_supersession, StoreError::Conflict(_)));

    let supersession_filter = MemoryFilter {
        tags: Some(vec![case_tag.clone(), "supersession".into()]),
        ..MemoryFilter::default()
    };
    let live_supersession_ids = ids(&store.list(supersession_filter.clone(), owner_ctx.clone()).await.unwrap());
    assert!(live_supersession_ids.contains(&new_id));
    assert!(!live_supersession_ids.contains(&old_id));
    let all_supersession_ids = ids(&store
        .list(
            MemoryFilter {
                include_superseded: Some(true),
                ..supersession_filter
            },
            owner_ctx.clone(),
        )
        .await
        .unwrap());
    assert!(all_supersession_ids.contains(&old_id));
    assert_eq!(superseded.superseded_by, Some(new_id));

    let neighbor = memory(MemorySpec {
        content: format!("near vector neighbor {case}"),
        tags: vec![case_tag.clone(), "neighbor".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 5),
    });
    let neighbor_id = store.store(&neighbor, Some(&embedding(embedding_dimensions, 0.1_f32))).await.unwrap();
    let superseded_neighbor = memory(MemorySpec {
        content: format!("superseded vector neighbor {case}"),
        tags: vec![case_tag.clone(), "neighbor".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 6),
    });
    let superseded_neighbor_id = store.store(&superseded_neighbor, Some(&embedding(embedding_dimensions, 0.05_f32))).await.unwrap();
    assert!(store.mark_superseded_by(&superseded_neighbor_id, &primary_id).await.unwrap());
    let neighbors = store.find_embedding_neighbors(&primary_embedding, 0.2_f64, 10).await.unwrap();
    assert!(neighbors.iter().any(|(id, distance)| *id == neighbor_id && *distance <= 0.2_f64));
    assert!(!neighbors.iter().any(|(id, _)| *id == superseded_neighbor_id));

    let batch_a = memory(MemorySpec {
        content: format!("batch a {case}"),
        tags: vec![case_tag.clone(), "batch".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 7),
    });
    let batch_b = memory(MemorySpec {
        content: format!("batch b {case}"),
        tags: vec![case_tag.clone(), "batch".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 8),
    });
    let batch_ids = store
        .store_batch(&[
            MemoryWithEmbedding {
                memory: batch_a.clone(),
                embedding: Some(embedding(embedding_dimensions, 7.0_f32)),
            },
            MemoryWithEmbedding {
                memory: batch_b.clone(),
                embedding: Some(embedding(embedding_dimensions, 7.1_f32)),
            },
        ])
        .await
        .unwrap();
    assert_eq!(batch_ids, vec![batch_a.id, batch_b.id]);

    let metadata_memory = memory(MemorySpec {
        content: format!("metadata {case}"),
        tags: vec![case_tag.clone(), "metadata".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 9),
    });
    let metadata = MemoryMetadata {
        memory_id: metadata_memory.id,
        scope_key: Some(scope.clone()),
        summary: Some("contract summary".into()),
        agent_label: Some("contract-agent-label".into()),
        created_by_principal: Some(OWNER.into()),
        quality_flags: vec!["contract_flag".into()],
        schema_version: 1,
    };
    let id = store
        .store_with_metadata(&metadata_memory, Some(&embedding(embedding_dimensions, 8.0_f32)), None, &metadata)
        .await
        .unwrap();
    assert_eq!(store.get_metadata(&id).await.unwrap(), Some(metadata));

    assert_reembed_claim_contract(store, embedding_dimensions, &case, base).await;
    assert_expired_cleanup_contract(store, &case, base).await;

    let update = MemoryUpdate {
        content: Some(format!("updated content {case}")),
        ..MemoryUpdate::default()
    };
    let updated = store.update_authorized(&primary_id, &update, OWNER).await.unwrap();
    assert_eq!(updated.outcome, WriteOutcome::Applied);
    assert!(updated.reembed_revision.is_some());

    let use_now = time_after(base, 13);
    let use_outcome = store
        .record_memory_use(&[id, primary_id, id, MemoryId::new()], OWNER, 1.0_f64, use_now, 24.0_f64)
        .await
        .unwrap();
    assert_eq!(use_outcome.recorded_ids, vec![id, primary_id]);
    assert_eq!(use_outcome.recorded, 2_u64);
    assert_eq!(use_outcome.not_found, 1_u64);
    let used = store.get(&primary_id, Some(OWNER)).await.unwrap().unwrap();
    assert_eq!(used.last_used_at, Some(use_now));

    store.record_search_impression(&[primary_id]).await.unwrap();
    assert!(store.get(&primary_id, Some(OWNER)).await.unwrap().unwrap().impression_count > 0_u64);

    let audit_time = time_after(base, 12);
    let details = json!({ "case": case_tag });
    store
        .write_audit_entry(&primary_id, AuditAction::Store, Some(OWNER), audit_time, Some(&details))
        .await
        .unwrap();
    let audit = store.query_audit_log(&primary_id, 10).await.unwrap();
    assert_eq!(audit.len(), 1_usize);
    assert_eq!(audit[0].action, AuditAction::Store);
    assert_eq!(audit[0].caller_agent.as_deref(), Some(OWNER));

    let audited = memory(MemorySpec {
        content: format!("audited transactional store {case}"),
        tags: vec![format!("audit-{case}")],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 12),
    });
    let audit_draft = AuditDraft {
        action: AuditAction::Store,
        caller_agent: Some(OWNER.into()),
        timestamp: time_after(base, 13),
        details: Some(json!({ "transactional": true })),
    };
    let audited_id = store.store_audited(&audited, None, &audit_draft).await.unwrap();
    let audited_history = store.query_audit_log(&audited_id, 10).await.unwrap();
    assert_eq!(audited_history.len(), 1_usize);
    assert_eq!(audited_history[0].action, AuditAction::Store);
    assert_eq!(audited_history[0].details, audit_draft.details);

    let original_audited_content = audited.content.clone();
    let content_after_audit_update = format!("audited transactional update {case}");
    let update_audit = AuditDraft {
        action: AuditAction::Update,
        caller_agent: Some(OWNER.into()),
        timestamp: time_after(base, 14),
        details: Some(json!({ "case": case_tag, "old_content_hash": "stale" })),
    };
    let content_update = MemoryUpdate {
        content: Some(content_after_audit_update.clone()),
        ..MemoryUpdate::default()
    };
    let content_outcome = store.update_authorized_audited(&audited_id, &content_update, OWNER, &update_audit).await.unwrap();
    assert_eq!(content_outcome.outcome, WriteOutcome::Applied);
    let content_history = store.query_audit_log(&audited_id, 10).await.unwrap();
    let update_audit_entry = content_history.iter().find(|entry| entry.timestamp == update_audit.timestamp).unwrap();
    let content_details = update_audit_entry.details.as_ref().and_then(serde_json::Value::as_object).unwrap();
    assert_eq!(content_details.get("old_content_hash"), Some(&json!(super::crud::content_hash(&original_audited_content))));
    assert_eq!(content_details.get("case"), Some(&json!(case_tag)));

    let metadata_patch = MetadataPatch {
        scope_key: Some(format!("contract/revised/{case}")),
        summary: Some(format!("revised summary {case}")),
        clear_summary: false,
        agent_label: Some("conformance-agent".into()),
        clear_agent_label: false,
    };
    let metadata_audit = AuditDraft {
        action: AuditAction::Update,
        caller_agent: Some(OWNER.into()),
        timestamp: time_after(base, 15),
        details: Some(json!({ "metadata": true, "old_content_hash": "stale" })),
    };
    let metadata_update = MemoryUpdate {
        content: Some(format!("audited transactional metadata update {case}")),
        tags: Some(vec![format!("revised-{case}")]),
        ..MemoryUpdate::default()
    };
    let metadata_outcome = store
        .update_authorized_with_metadata_audited(&audited_id, &metadata_update, Some(&metadata_patch), OWNER, &metadata_audit)
        .await
        .unwrap();
    assert_eq!(metadata_outcome.outcome, WriteOutcome::Applied);
    let revised_metadata = store.get_metadata(&audited_id).await.unwrap().unwrap();
    assert_eq!(revised_metadata.scope_key, metadata_patch.scope_key);
    assert_eq!(revised_metadata.summary, metadata_patch.summary);
    let metadata_history = store.query_audit_log(&audited_id, 10).await.unwrap();
    let metadata_audit_entry = metadata_history.iter().find(|entry| entry.timestamp == metadata_audit.timestamp).unwrap();
    let metadata_details = metadata_audit_entry.details.as_ref().and_then(serde_json::Value::as_object).unwrap();
    assert_eq!(metadata_details.get("metadata"), Some(&json!(true)));
    assert_eq!(
        metadata_details.get("old_content_hash"),
        Some(&json!(super::crud::content_hash(&content_after_audit_update)))
    );

    let interactive = memory(MemorySpec {
        content: format!("interactive original {case}"),
        tags: vec![case_tag.clone(), "interactive".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 16),
    });
    let interactive_id = store.store(&interactive, Some(&embedding(embedding_dimensions, 20.0_f32))).await.unwrap();
    let interactive_metadata = MemoryMetadata {
        memory_id: interactive_id,
        scope_key: Some(scope.clone()),
        summary: Some("interactive summary".into()),
        agent_label: Some("interactive agent".into()),
        created_by_principal: Some(OWNER.into()),
        quality_flags: Vec::new(),
        schema_version: 1,
    };
    store.upsert_metadata(interactive_metadata.clone()).await.unwrap();
    let opened_before_external_update = store.get(&interactive_id, Some(OWNER)).await.unwrap().unwrap();
    let concurrency_audit = AuditDraft {
        action: AuditAction::Update,
        caller_agent: Some(OWNER.into()),
        timestamp: time_after(base, 16),
        details: Some(json!({"concurrency": true})),
    };

    let ordinary_outcome = store
        .update_authorized(
            &interactive_id,
            &MemoryUpdate {
                tags: Some(vec![case_tag.clone(), "external-tag-revision".into()]),
                ..MemoryUpdate::default()
            },
            OWNER,
        )
        .await
        .unwrap();
    assert_eq!(ordinary_outcome.outcome, WriteOutcome::Applied);
    let after_ordinary_update = store.get(&interactive_id, Some(OWNER)).await.unwrap().unwrap();
    assert!(
        after_ordinary_update.record_revision > opened_before_external_update.record_revision,
        "ordinary non-content updates must advance the optimistic revision"
    );
    assert_eq!(
        after_ordinary_update.updated_at, opened_before_external_update.updated_at,
        "ordinary non-content updates must preserve content freshness"
    );
    let stale_after_ordinary_update = store
        .update_authorized_if_unmodified_with_metadata_audited(
            &interactive_id,
            opened_before_external_update.record_revision,
            &MemoryUpdate {
                importance: Some(Importance::new(0.6_f64)),
                ..MemoryUpdate::default()
            },
            None,
            None,
            OWNER,
            &concurrency_audit,
        )
        .await
        .unwrap_err();
    assert!(matches!(stale_after_ordinary_update, StoreError::Conflict(_)));

    let mut concurrent_metadata = interactive_metadata;
    concurrent_metadata.summary = Some("external metadata revision".into());
    store.upsert_metadata(concurrent_metadata).await.unwrap();
    let loaded = store.get(&interactive_id, Some(OWNER)).await.unwrap().unwrap();
    assert!(
        loaded.record_revision > after_ordinary_update.record_revision,
        "standalone metadata upserts must advance the optimistic revision"
    );
    assert_eq!(
        loaded.updated_at, after_ordinary_update.updated_at,
        "standalone metadata upserts must preserve content freshness"
    );
    let stale_after_metadata_update = store
        .update_authorized_if_unmodified_with_metadata_audited(
            &interactive_id,
            after_ordinary_update.record_revision,
            &MemoryUpdate {
                importance: Some(Importance::new(0.7_f64)),
                ..MemoryUpdate::default()
            },
            None,
            None,
            OWNER,
            &concurrency_audit,
        )
        .await
        .unwrap_err();
    assert!(matches!(stale_after_metadata_update, StoreError::Conflict(_)));

    let replacement_content = format!("interactive revised {case}");
    let replacement_expiry = Utc.with_ymd_and_hms(2099, 1, 1, 0, 0, 0).single().unwrap();
    let replacement_update = MemoryUpdate {
        content: Some(replacement_content.clone()),
        tags: Some(vec![case_tag.clone(), "revised-interactive".into()]),
        expires_at: Some(Some(replacement_expiry)),
        importance: Some(Importance::new(0.9_f64)),
        ..MemoryUpdate::default()
    };
    let replacement_metadata = MetadataPatch {
        scope_key: None,
        summary: None,
        clear_summary: true,
        agent_label: Some("revised agent".into()),
        clear_agent_label: false,
    };
    let replacement_audit = AuditDraft {
        action: AuditAction::Update,
        caller_agent: Some(OWNER.into()),
        timestamp: time_after(base, 17),
        details: Some(json!({"interactive": true})),
    };
    let wrong_dimension_error = store
        .update_authorized_if_unmodified_with_metadata_audited(
            &interactive_id,
            loaded.record_revision,
            &MemoryUpdate {
                content: Some("wrong-dimensional revision".into()),
                ..MemoryUpdate::default()
            },
            None,
            Some(&vec![0.0_f32; embedding_dimensions.saturating_add(1_usize)]),
            OWNER,
            &replacement_audit,
        )
        .await
        .unwrap_err();
    assert!(matches!(wrong_dimension_error, StoreError::Conflict(_)));
    assert_eq!(store.get(&interactive_id, Some(OWNER)).await.unwrap().unwrap().content, interactive.content);

    let replacement_embedding = embedding(embedding_dimensions, 21.0_f32);
    let interactive_outcome = store
        .update_authorized_if_unmodified_with_metadata_audited(
            &interactive_id,
            loaded.record_revision,
            &replacement_update,
            Some(&replacement_metadata),
            Some(&replacement_embedding),
            OWNER,
            &replacement_audit,
        )
        .await
        .unwrap();
    assert_eq!(interactive_outcome.outcome, WriteOutcome::Applied);
    assert!(interactive_outcome.reembed_revision.is_none());
    let revised = store.get(&interactive_id, Some(OWNER)).await.unwrap().unwrap();
    assert_eq!(revised.content, replacement_content);
    assert_eq!(revised.expires_at, Some(replacement_expiry));
    assert!(revised.has_embedding);
    assert_eq!(
        store.fetch_embeddings_for_ids(&[interactive_id]).await.unwrap().get(&interactive_id),
        Some(&replacement_embedding)
    );
    let revised_metadata = store.get_metadata(&interactive_id).await.unwrap().unwrap();
    assert!(revised_metadata.summary.is_none());
    assert_eq!(revised_metadata.agent_label.as_deref(), Some("revised agent"));

    let metadata_only_outcome = store
        .update_authorized_if_unmodified_with_metadata_audited(
            &interactive_id,
            revised.record_revision,
            &MemoryUpdate::default(),
            Some(&MetadataPatch {
                scope_key: None,
                summary: Some("metadata revision".into()),
                clear_summary: false,
                agent_label: None,
                clear_agent_label: false,
            }),
            None,
            OWNER,
            &replacement_audit,
        )
        .await
        .unwrap();
    assert_eq!(metadata_only_outcome.outcome, WriteOutcome::Applied);
    let metadata_revised = store.get(&interactive_id, Some(OWNER)).await.unwrap().unwrap();
    assert!(
        metadata_revised.record_revision > revised.record_revision,
        "metadata-only interactive edits must advance the optimistic revision"
    );
    assert_eq!(
        metadata_revised.updated_at, revised.updated_at,
        "metadata-only interactive edits must preserve content freshness"
    );
    let stale_metadata_error = store
        .update_authorized_if_unmodified_with_metadata_audited(
            &interactive_id,
            revised.record_revision,
            &MemoryUpdate {
                importance: Some(Importance::new(0.4_f64)),
                ..MemoryUpdate::default()
            },
            None,
            None,
            OWNER,
            &replacement_audit,
        )
        .await
        .unwrap_err();
    assert!(matches!(stale_metadata_error, StoreError::Conflict(_)));

    let stale_error = store
        .update_authorized_if_unmodified_with_metadata_audited(
            &interactive_id,
            loaded.record_revision,
            &MemoryUpdate {
                content: Some("stale overwrite".into()),
                ..MemoryUpdate::default()
            },
            None,
            Some(&embedding(embedding_dimensions, 22.0_f32)),
            OWNER,
            &replacement_audit,
        )
        .await
        .unwrap_err();
    assert!(matches!(stale_error, StoreError::Conflict(_)));
    assert_eq!(store.get(&interactive_id, Some(OWNER)).await.unwrap().unwrap().content, replacement_content);

    let delete_audit = AuditDraft {
        action: AuditAction::Delete,
        caller_agent: Some(OWNER.into()),
        timestamp: time_after(base, 18),
        details: Some(json!({"interactive": true})),
    };
    let stale_delete = store
        .delete_authorized_if_unmodified_audited(&interactive_id, loaded.record_revision, OWNER, &delete_audit)
        .await
        .unwrap_err();
    assert!(matches!(stale_delete, StoreError::Conflict(_)));
    let delete_outcome = store
        .delete_authorized_if_unmodified_audited(&interactive_id, metadata_revised.record_revision, OWNER, &delete_audit)
        .await
        .unwrap();
    assert_eq!(delete_outcome, WriteOutcome::Applied);
    assert!(store.get(&interactive_id, Some(OWNER)).await.unwrap().is_none());

    let from_scope = format!("contract/from/{case}");
    let to_scope = format!("contract/to/{case}");
    store
        .register_scope_for_principal(
            ScopeDefinition {
                scope_key: to_scope.clone(),
                display_name: "Canonical reassignment target".into(),
                description: None,
                aliases: Vec::new(),
                matchers: Vec::new(),
                parent: None,
                related: Vec::new(),
            },
            OWNER,
        )
        .await
        .unwrap();
    let movable = memory(MemorySpec {
        content: format!("movable scope {case}"),
        tags: vec![case_tag.clone(), "move".into()],
        source_agent: OWNER,
        scope: from_scope.clone(),
        origin,
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 13),
    });
    let movable_id = store.store(&movable, Some(&embedding(embedding_dimensions, 10.0_f32))).await.unwrap();
    let opened_movable = store.get(&movable_id, Some(OWNER)).await.unwrap().unwrap();
    let requested_to_scope = format!(" {} ", to_scope.to_uppercase());
    let reassigned = store.reassign_scope(&from_scope, &requested_to_scope, None, OWNER).await.unwrap();
    assert_eq!(reassigned.applied_ids, vec![movable_id]);
    let moved = store.get(&movable_id, Some(OWNER)).await.unwrap().unwrap();
    assert_eq!(moved.provenance.source_conversation.as_deref(), Some(to_scope.as_str()));
    assert!(
        moved.record_revision > opened_movable.record_revision,
        "scope reassignment must advance the optimistic revision"
    );
    assert_eq!(moved.updated_at, opened_movable.updated_at, "scope reassignment must preserve content freshness");
    let moved_contexts = store.get_memory_contexts(&movable_id, OWNER).await.unwrap();
    assert_eq!(moved_contexts.len(), 1);
    assert_eq!(moved_contexts[0].context.key, to_scope);
    assert_eq!(moved_contexts[0].context.owner_principal, OWNER);
    assert_eq!(moved_contexts[0].context.kind.as_str(), ContextKind::CUSTOM);

    let delete_me = memory(MemorySpec {
        content: format!("delete me {case}"),
        tags: vec![case_tag, "delete".into()],
        source_agent: OWNER,
        scope,
        origin: format!("contract/delete-origin/{case}"),
        access_policy: AccessPolicy::Restricted { allowed: vec![ALLOWED.into()] },
        created_at: time_after(base, 14),
    });
    let delete_id = store.store(&delete_me, Some(&embedding(embedding_dimensions, 11.0_f32))).await.unwrap();
    assert_eq!(store.delete_authorized(&delete_id, OWNER).await.unwrap(), WriteOutcome::Applied);
    assert!(store.get(&delete_id, Some(OWNER)).await.unwrap().is_none());
    let tombstone = store.get_tombstone(&delete_id).await.unwrap().unwrap();
    assert_eq!(tombstone.memory_id, delete_id);
    assert_eq!(tombstone.deleted_by_principal.as_deref(), Some(OWNER));

    let context_specs = [
        ("project", format!("project/x/{case}"), "Project X"),
        ("project", format!("project/y/{case}"), "Project Y"),
        ("domain", format!("domain/architecture/{case}"), "Architecture"),
        ("domain", format!("domain/operations/{case}"), "Operations"),
    ];
    let mut governed = Vec::new();
    for (kind, key, display_name) in context_specs {
        let id = ContextId::new();
        let draft = ContextCreateDraft {
            id,
            kind: ContextKind::new(kind).unwrap(),
            normalized_key: crate::types::normalize_context_key(&key),
            key,
            display_name: display_name.into(),
            description: None,
            owner_principal: OWNER.into(),
            guidance: None,
            parent_id: None,
            aliases: Vec::new(),
            identities: Vec::new(),
            resolver_hints: Vec::new(),
            confirm_distinct_from: Vec::new(),
            enforce_fuzzy_confirmation: false,
            frozen: false,
        };
        let audit = ContextAuditDraft {
            actor_principal: OWNER.into(),
            action: "conformance_context_created".into(),
            context_id: Some(id),
            memory_id: None,
            details: None,
        };
        let created = store.create_context(&draft, &audit).await.unwrap();
        governed.push(created.id);
    }
    let project_x = governed[0];
    let project_y = governed[1];
    let architecture = governed[2];
    let operations = governed[3];
    let optimistic_context_memory = memory(MemorySpec {
        content: format!("optimistic context replacement {case}"),
        tags: vec![format!("optimistic-context-{case}")],
        source_agent: OWNER,
        scope: store.get_context(&project_x, OWNER).await.unwrap().unwrap().key,
        origin: format!("contract/optimistic-context/{case}"),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 29),
    });
    let optimistic_context_id = store.store(&optimistic_context_memory, None).await.unwrap();
    let opened_optimistic = store.get(&optimistic_context_id, Some(OWNER)).await.unwrap().unwrap();
    let optimistic_audit = AuditDraft {
        action: AuditAction::Update,
        caller_agent: Some(OWNER.into()),
        timestamp: time_after(base, 30),
        details: Some(json!({"contexts_replaced": true})),
    };
    let optimistic_context_audit = ContextAuditDraft {
        actor_principal: OWNER.into(),
        action: "conformance_optimistic_contexts_replaced".into(),
        context_id: None,
        memory_id: Some(optimistic_context_id),
        details: None,
    };
    let optimistic_outcome = store
        .update_authorized_if_unmodified_with_metadata_contexts_audited(
            &optimistic_context_id,
            opened_optimistic.record_revision,
            &MemoryUpdate::default(),
            None,
            Some(&[project_y]),
            None,
            OWNER,
            &optimistic_audit,
            Some(&optimistic_context_audit),
        )
        .await
        .unwrap();
    assert_eq!(optimistic_outcome.outcome, WriteOutcome::Applied);
    let project_y_key = store.get_context(&project_y, OWNER).await.unwrap().unwrap().key;
    assert_eq!(
        store.get_metadata(&optimistic_context_id).await.unwrap().unwrap().scope_key.as_deref(),
        Some(project_y_key.as_str())
    );
    assert_eq!(
        store
            .get(&optimistic_context_id, Some(OWNER))
            .await
            .unwrap()
            .unwrap()
            .provenance
            .source_conversation
            .as_deref(),
        Some(project_y_key.as_str())
    );
    let stale_revision = store.get(&optimistic_context_id, Some(OWNER)).await.unwrap().unwrap().record_revision;
    assert_eq!(
        store
            .replace_memory_contexts(&optimistic_context_id, &[architecture], OWNER, &ContextAuditDraft {
                actor_principal: OWNER.into(),
                action: "conformance_concurrent_contexts_replaced".into(),
                context_id: None,
                memory_id: Some(optimistic_context_id),
                details: None,
            })
            .await
            .unwrap(),
        WriteOutcome::Applied
    );
    let stale_error = store
        .update_authorized_if_unmodified_with_metadata_contexts_audited(
            &optimistic_context_id,
            stale_revision,
            &MemoryUpdate::default(),
            None,
            Some(&[project_y]),
            None,
            OWNER,
            &optimistic_audit,
            Some(&optimistic_context_audit),
        )
        .await
        .unwrap_err();
    assert!(stale_error.to_string().contains("changed after it was opened"), "{stale_error}");
    assert_eq!(
        store
            .get_memory_contexts(&optimistic_context_id, OWNER)
            .await
            .unwrap()
            .iter()
            .map(|membership| membership.context.id)
            .collect::<Vec<_>>(),
        vec![architecture]
    );
    let fuzzy_domain_id = ContextId::new();
    let mut fuzzy_domain = ContextCreateDraft::private(
        fuzzy_domain_id,
        ContextKind::new(ContextKind::DOMAIN).unwrap(),
        format!("domain/architecture-copy/{case}"),
        "Architecture",
        OWNER,
    );
    fuzzy_domain.enforce_fuzzy_confirmation = true;
    let fuzzy_error = store
        .create_context(
            &fuzzy_domain,
            &ContextAuditDraft::new(OWNER, "conformance_fuzzy_domain_rejected").with_context(fuzzy_domain_id),
        )
        .await
        .unwrap_err();
    assert!(fuzzy_error.to_string().contains("fuzzy context candidates changed"));
    fuzzy_domain.confirm_distinct_from = vec![architecture];
    let _fuzzy_domain = store
        .create_context(
            &fuzzy_domain,
            &ContextAuditDraft::new(OWNER, "conformance_fuzzy_domain_confirmed").with_context(fuzzy_domain_id),
        )
        .await
        .unwrap();
    let archived_legacy_key = format!("legacy/archived/{case}");
    let archived_legacy_scope = ScopeDefinition {
        scope_key: archived_legacy_key.clone(),
        display_name: "Archived legacy adapter".into(),
        description: None,
        aliases: Vec::new(),
        matchers: Vec::new(),
        parent: None,
        related: Vec::new(),
    };
    store.register_scope_for_principal(archived_legacy_scope.clone(), OWNER).await.unwrap();
    let archived_record = store
        .find_context_records(OWNER, false, &ContextExactLookup::Key {
            kind: Some(ContextKind::custom()),
            normalized_key: crate::types::normalize_context_key(&archived_legacy_key),
        })
        .await
        .unwrap()
        .pop()
        .unwrap();
    store
        .set_context_lifecycle(
            &archived_record.context.id,
            ContextLifecycle::Archived,
            OWNER,
            &ContextAuditDraft::new(OWNER, "conformance_legacy_scope_archived").with_context(archived_record.context.id),
        )
        .await
        .unwrap();
    let archived_registration_error = store.register_scope_for_principal(archived_legacy_scope, OWNER).await.unwrap_err();
    assert!(archived_registration_error.to_string().contains("reactivate it in the TUI"));
    assert!(
        store
            .list_scopes_for_principal(OWNER)
            .await
            .unwrap()
            .iter()
            .all(|scope| scope.scope_key != archived_legacy_key)
    );
    let context_audit = |action: &str, context_id: Option<ContextId>| ContextAuditDraft {
        actor_principal: OWNER.into(),
        action: action.into(),
        context_id,
        memory_id: None,
        details: None,
    };
    store
        .update_context_definition(
            &project_x,
            &ContextDefinitionPatch {
                display_name: "Project X renamed".into(),
                description: Some("managed definition".into()),
                guidance: Some("Use for project X work.".into()),
                aliases: vec![format!("project-x-alias-{case}")],
                identities: Vec::new(),
                resolver_hints: vec![format!("project-x-hint-{case}")],
            },
            OWNER,
            &context_audit("conformance_context_updated", Some(project_x)),
        )
        .await
        .unwrap();
    let project_x_record = store
        .list_context_records(OWNER, false, 0, 500)
        .await
        .unwrap()
        .into_iter()
        .find(|record| record.context.id == project_x)
        .unwrap();
    assert_eq!(project_x_record.context.display_name, "Project X renamed");
    assert_eq!(project_x_record.aliases, vec![format!("project-x-alias-{case}")]);

    store
        .grant_context_use(&project_x, VIEWER, OWNER, &context_audit("conformance_context_granted", Some(project_x)))
        .await
        .unwrap();
    assert!(store.get_context(&project_x, VIEWER).await.unwrap().is_some());
    assert_eq!(store.list_context_grants(&project_x, OWNER).await.unwrap().len(), 1);
    assert!(store.list_context_grants(&project_x, VIEWER).await.unwrap().is_empty());
    store
        .revoke_context_use(&project_x, VIEWER, OWNER, &context_audit("conformance_context_grant_revoked", Some(project_x)))
        .await
        .unwrap();
    assert!(store.get_context(&project_x, VIEWER).await.unwrap().is_none());
    let replacement_grantees = vec![VIEWER.to_owned(), ALLOWED.to_owned()];
    store
        .replace_context_grants(
            &project_x,
            &replacement_grantees,
            OWNER,
            &context_audit("conformance_context_grants_replaced", Some(project_x)),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .list_context_grants(&project_x, OWNER)
            .await
            .unwrap()
            .into_iter()
            .map(|grant| grant.grantee_principal)
            .collect::<BTreeSet<_>>(),
        replacement_grantees.into_iter().collect()
    );
    let duplicate_grantees = vec![VIEWER.to_owned(), format!(" {VIEWER} ")];
    assert!(
        store
            .replace_context_grants(
                &project_x,
                &duplicate_grantees,
                OWNER,
                &context_audit("conformance_context_grants_invalid", Some(project_x)),
            )
            .await
            .is_err()
    );
    assert_eq!(store.list_context_grants(&project_x, OWNER).await.unwrap().len(), 2);

    let lineage_viewer = format!("lineage-viewer-{case}");
    store
        .set_context_parent(&operations, Some(&architecture), OWNER, &context_audit("conformance_context_parent_set", Some(operations)))
        .await
        .unwrap();
    store
        .grant_context_use(&operations, &lineage_viewer, OWNER, &context_audit("conformance_child_context_granted", Some(operations)))
        .await
        .unwrap();
    store
        .grant_context_use(
            &architecture,
            &lineage_viewer,
            OWNER,
            &context_audit("conformance_parent_context_granted", Some(architecture)),
        )
        .await
        .unwrap();
    let child_selection = store.expand_context_selection(&[operations], &lineage_viewer, false).await.unwrap();
    assert_eq!(
        child_selection.iter().map(|context| context.id).collect::<BTreeSet<_>>(),
        [operations, architecture].into_iter().collect(),
        "an authorized child selection must include its authorized ancestor chain"
    );

    let foreign_owner = format!("foreign-owner-{case}");
    let foreign_parent = ContextId::new();
    let _foreign_context = store
        .create_context(
            &ContextCreateDraft::private(
                foreign_parent,
                ContextKind::new(ContextKind::DOMAIN).unwrap(),
                format!("domain/foreign-parent/{case}"),
                "Foreign parent",
                &foreign_owner,
            ),
            &ContextAuditDraft::new(&foreign_owner, "conformance_foreign_parent_created").with_context(foreign_parent),
        )
        .await
        .unwrap();
    store
        .grant_context_use(
            &foreign_parent,
            OWNER,
            &foreign_owner,
            &ContextAuditDraft::new(&foreign_owner, "conformance_foreign_parent_granted").with_context(foreign_parent),
        )
        .await
        .unwrap();
    store
        .set_context_parent(
            &project_y,
            Some(&foreign_parent),
            OWNER,
            &context_audit("conformance_foreign_parent_selected", Some(project_y)),
        )
        .await
        .unwrap();
    store
        .revoke_context_use(
            &foreign_parent,
            OWNER,
            &foreign_owner,
            &ContextAuditDraft::new(&foreign_owner, "conformance_foreign_parent_revoked").with_context(foreign_parent),
        )
        .await
        .unwrap();
    let unavailable_lineage = store.expand_context_selection(&[project_y], OWNER, false).await.unwrap_err();
    assert!(unavailable_lineage.to_string().contains("unavailable parent"));

    let case_prefix = case.chars().take(8).collect::<String>();
    let user_kind = ContextKind::new(format!("release_train_{case_prefix}")).unwrap();
    store
        .upsert_context_kind(
            &ContextKindDraft {
                kind: user_kind.clone(),
                display_name: "Release train".into(),
                enabled: true,
            },
            OWNER,
            &context_audit("conformance_context_kind_upserted", None),
        )
        .await
        .unwrap();
    assert!(store.list_context_kinds().await.unwrap().iter().any(|definition| definition.kind == user_kind));

    let policy_guarded_legacy_id = ContextId::new();
    let _policy_guarded_context = store
        .create_context(
            &ContextCreateDraft::private(
                policy_guarded_legacy_id,
                ContextKind::custom(),
                format!("legacy/policy-default/{case}"),
                "Policy default legacy compatibility context",
                OWNER,
            ),
            &ContextAuditDraft::new(OWNER, "legacy_scope_context_created").with_context(policy_guarded_legacy_id),
        )
        .await
        .unwrap();
    store
        .upsert_context_kind_policy(
            &ContextKindPolicyDraft {
                layer: ContextPolicyLayer::Principal,
                principal: OWNER.into(),
                kind: ContextKind::custom(),
                policy: ContextKindPolicy {
                    default_context_id: Some(policy_guarded_legacy_id),
                    ..ContextKindPolicy::default()
                },
            },
            OWNER,
            &context_audit("conformance_policy_default_guarded", None),
        )
        .await
        .unwrap();
    assert!(
        !store.rollback_unreferenced_legacy_context(&policy_guarded_legacy_id, OWNER).await.unwrap(),
        "compatibility cleanup must preserve contexts referenced by policy defaults"
    );
    assert!(store.get_context(&policy_guarded_legacy_id, OWNER).await.unwrap().is_some());

    let anchor_guarded_legacy_id = ContextId::new();
    let _anchor_guarded_context = store
        .create_context(
            &ContextCreateDraft::private(
                anchor_guarded_legacy_id,
                ContextKind::custom(),
                format!("legacy/anchor-default/{case}"),
                "Anchor default legacy compatibility context",
                OWNER,
            ),
            &ContextAuditDraft::new(OWNER, "legacy_scope_context_created").with_context(anchor_guarded_legacy_id),
        )
        .await
        .unwrap();
    let mut guarded_anchor_policy = ContextAnchorPolicy::default();
    let _previous = guarded_anchor_policy.kinds.insert(ContextKind::CUSTOM.into(), ContextKindPolicy {
        default_context_id: Some(anchor_guarded_legacy_id),
        ..ContextKindPolicy::default()
    });
    store
        .upsert_context_anchor_policy(
            &ContextAnchorPolicyDraft {
                anchor_context_id: project_x,
                principal: OWNER.into(),
                policy: guarded_anchor_policy,
            },
            OWNER,
            &context_audit("conformance_anchor_default_guarded", Some(project_x)),
        )
        .await
        .unwrap();
    assert!(
        !store.rollback_unreferenced_legacy_context(&anchor_guarded_legacy_id, OWNER).await.unwrap(),
        "compatibility cleanup must preserve contexts referenced by nested anchor defaults"
    );
    assert!(store.get_context(&anchor_guarded_legacy_id, OWNER).await.unwrap().is_some());

    let operator_policy = ContextKindPolicyDraft {
        layer: ContextPolicyLayer::Operator,
        principal: String::new(),
        kind: ContextKind::new(ContextKind::DOMAIN).unwrap(),
        policy: ContextKindPolicy {
            required: Some(true),
            ..ContextKindPolicy::default()
        },
    };
    let unauthorized_operator_policy = store
        .upsert_context_kind_policy(&operator_policy, OWNER, &context_audit("conformance_operator_policy_denied", None))
        .await
        .unwrap_err();
    assert!(unauthorized_operator_policy.to_string().contains(OPERATOR_PRINCIPAL));
    store
        .upsert_context_kind_policy(
            &operator_policy,
            OPERATOR_PRINCIPAL,
            &ContextAuditDraft::new(OPERATOR_PRINCIPAL, "conformance_operator_policy_upserted"),
        )
        .await
        .unwrap();

    store
        .upsert_context_kind_policy(
            &ContextKindPolicyDraft {
                layer: ContextPolicyLayer::Principal,
                principal: OWNER.into(),
                kind: ContextKind::new(ContextKind::DOMAIN).unwrap(),
                policy: ContextKindPolicy {
                    agent_creation: Some(true),
                    default_context_id: Some(architecture),
                    guidance: Some("Conformance principal policy".into()),
                    ..ContextKindPolicy::default()
                },
            },
            OWNER,
            &context_audit("conformance_context_policy_upserted", None),
        )
        .await
        .unwrap();
    let policies = store.list_context_kind_policies(OWNER).await.unwrap();
    assert!(policies.iter().any(|record| record.kind.as_str() == ContextKind::DOMAIN));

    let mut anchor_kinds = BTreeMap::new();
    let _old = anchor_kinds.insert(ContextKind::DOMAIN.into(), ContextKindPolicy {
        include_descendants: Some(true),
        guidance: Some("Conformance anchor policy".into()),
        ..ContextKindPolicy::default()
    });
    store
        .upsert_context_anchor_policy(
            &ContextAnchorPolicyDraft {
                anchor_context_id: project_x,
                principal: OWNER.into(),
                policy: ContextAnchorPolicy { kinds: anchor_kinds },
            },
            OWNER,
            &context_audit("conformance_anchor_policy_upserted", Some(project_x)),
        )
        .await
        .unwrap();
    assert_eq!(store.list_context_anchor_policies(OWNER).await.unwrap().len(), 1);
    assert!(!store.query_context_audit(&project_x, OWNER, 20).await.unwrap().is_empty());

    let applicability_tag = format!("applicability-{case}");
    let membership_sets = [
        ("project x", vec![project_x]),
        ("project y", vec![project_y]),
        ("architecture", vec![architecture]),
        ("operations", vec![operations]),
        ("project x architecture", vec![project_x, architecture]),
        ("project x operations", vec![project_x, operations]),
    ];
    let mut applicability_ids = Vec::new();
    let applicability_embedding = embedding(embedding_dimensions, 12.0_f32);
    for (index, (label, context_ids)) in membership_sets.into_iter().enumerate() {
        let primary_key = store.get_context(&context_ids[0], OWNER).await.unwrap().unwrap().key;
        let memory = memory(MemorySpec {
            content: format!("{label} applicability {case}"),
            tags: vec![applicability_tag.clone()],
            source_agent: OWNER,
            scope: primary_key.clone(),
            origin: format!("contract/applicability/{case}/{index}"),
            access_policy: AccessPolicy::Public,
            created_at: time_after(base, 30_i64.saturating_add(i64::try_from(index).unwrap())),
        });
        let metadata = MemoryMetadata {
            memory_id: memory.id,
            scope_key: Some(primary_key),
            summary: None,
            agent_label: None,
            created_by_principal: Some(OWNER.into()),
            quality_flags: Vec::new(),
            schema_version: 1,
        };
        let audit = AuditDraft {
            action: AuditAction::Store,
            caller_agent: Some(OWNER.into()),
            timestamp: memory.created_at,
            details: None,
        };
        let context_audit = ContextAuditDraft {
            actor_principal: OWNER.into(),
            action: "conformance_memberships_initialized".into(),
            context_id: None,
            memory_id: Some(memory.id),
            details: None,
        };
        let id = store
            .store_with_metadata_contexts_audited(&memory, Some(&applicability_embedding), None, &metadata, &context_ids, &audit, &context_audit)
            .await
            .unwrap();
        applicability_ids.push(id);
    }
    assert!(store.get(&applicability_ids[1], Some(VIEWER)).await.unwrap().is_some());
    assert!(
        store.get_memory_contexts(&applicability_ids[1], VIEWER).await.unwrap().is_empty(),
        "a readable public memory must not disclose private context metadata"
    );
    let applicability_filter = MemoryFilter {
        tags: Some(vec![applicability_tag]),
        context_ids: Some(vec![project_x, architecture]),
        limit: Some(20),
        ..MemoryFilter::default()
    };
    let applicable = store
        .list(applicability_filter.clone(), owner_ctx.clone())
        .await
        .unwrap()
        .into_iter()
        .map(|memory| memory.id)
        .collect::<BTreeSet<_>>();
    let expected = [applicability_ids[0], applicability_ids[2], applicability_ids[4]].into_iter().collect::<BTreeSet<_>>();
    assert_eq!(applicable, expected);
    let text_applicable = store
        .search_by_text("applicability", 20, &applicability_filter, &owner_ctx)
        .await
        .unwrap()
        .into_iter()
        .map(|result| result.memory.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(text_applicable, expected);
    if store.fts_available() {
        let fts_applicable = store
            .search_by_fts("applicability", 20, &applicability_filter, &owner_ctx, None)
            .await
            .unwrap()
            .into_iter()
            .map(|result| result.memory.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(fts_applicable, expected);
    }
    let vector_applicable = store
        .search_by_embedding(&applicability_embedding, 20, &applicability_filter, &owner_ctx, Some(0.001_f64))
        .await
        .unwrap()
        .into_iter()
        .map(|result| result.memory.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(vector_applicable, expected);

    let legacy_any_filter = MemoryFilter {
        tags: applicability_filter.tags.clone(),
        legacy_context_ids_any: Some(vec![project_x, architecture]),
        limit: Some(20),
        ..MemoryFilter::default()
    };
    let legacy_any = store
        .list(legacy_any_filter.clone(), owner_ctx.clone())
        .await
        .unwrap()
        .into_iter()
        .map(|memory| memory.id)
        .collect::<BTreeSet<_>>();
    let legacy_any_expected = [applicability_ids[0], applicability_ids[2], applicability_ids[4], applicability_ids[5]]
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(
        legacy_any, legacy_any_expected,
        "legacy scopes must remain OR alternatives even when their governed kinds differ"
    );
    let legacy_any_vector = store
        .search_by_embedding(&applicability_embedding, 20, &legacy_any_filter, &owner_ctx, Some(0.001_f64))
        .await
        .unwrap()
        .into_iter()
        .map(|result| result.memory.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        legacy_any_vector, legacy_any_expected,
        "legacy any-match membership must be applied before vector candidate work"
    );
    let legacy_any_embeddings = store
        .list_with_embeddings(None, legacy_any_filter.legacy_context_ids_any.as_deref(), OWNER, 20)
        .await
        .unwrap()
        .into_iter()
        .map(|candidate| candidate.memory.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        legacy_any_embeddings, legacy_any_expected,
        "legacy any-match membership must also constrain consolidation candidates"
    );

    store
        .set_context_lifecycle(
            &operations,
            ContextLifecycle::Archived,
            OWNER,
            &context_audit("conformance_operations_archived", Some(operations)),
        )
        .await
        .unwrap();
    let archived_selection = MemoryFilter {
        context_ids: Some(vec![operations]),
        limit: Some(20),
        ..applicability_filter.clone()
    };
    assert!(
        store.list(archived_selection, owner_ctx.clone()).await.unwrap().is_empty(),
        "an archived selected context must not satisfy retrieval"
    );
    assert!(
        store.list_with_embeddings(Some(&[operations]), None, OWNER, 20).await.unwrap().is_empty(),
        "consolidation candidates must reject an archived selected context"
    );
    let active_project_with_archived_companion = MemoryFilter {
        context_ids: Some(vec![project_x, operations]),
        limit: Some(20),
        ..applicability_filter
    };
    let active_results = store
        .list(active_project_with_archived_companion, owner_ctx)
        .await
        .unwrap()
        .into_iter()
        .map(|memory| memory.id)
        .collect::<BTreeSet<_>>();
    let active_expected = [applicability_ids[0], applicability_ids[5]].into_iter().collect::<BTreeSet<_>>();
    assert_eq!(
        active_results, active_expected,
        "archived companion memberships must not add an active applicability constraint"
    );
    let active_embedding_ids = store
        .list_with_embeddings(Some(&[project_x, operations]), None, OWNER, 100)
        .await
        .unwrap()
        .into_iter()
        .map(|candidate| candidate.memory.id)
        .collect::<BTreeSet<_>>();
    assert!(active_embedding_ids.contains(&applicability_ids[0]));
    assert!(active_embedding_ids.contains(&applicability_ids[5]));
    assert!(!active_embedding_ids.contains(&applicability_ids[3]));
}

struct WritePolicyCase {
    name: &'static str,
    source_agent: Option<&'static str>,
    access_policy: AccessPolicy,
    authorized: bool,
}

impl WritePolicyCase {
    fn new(name: &'static str, source_agent: Option<&'static str>, access_policy: AccessPolicy, authorized: bool) -> Self {
        Self {
            name,
            source_agent,
            access_policy,
            authorized,
        }
    }
}

fn write_policy_cases() -> [WritePolicyCase; 9] {
    [
        WritePolicyCase::new("public-non-owner", Some(VIEWER), AccessPolicy::Public, false),
        WritePolicyCase::new("public-owner", Some(OWNER), AccessPolicy::Public, true),
        WritePolicyCase::new("public-ownerless", None, AccessPolicy::Public, true),
        WritePolicyCase::new("restricted-owner", Some(OWNER), AccessPolicy::Restricted { allowed: Vec::new() }, true),
        WritePolicyCase::new(
            "restricted-allowlisted-non-owner",
            Some(VIEWER),
            AccessPolicy::Restricted { allowed: vec![OWNER.into()] },
            true,
        ),
        WritePolicyCase::new("restricted-denied", Some(VIEWER), AccessPolicy::Restricted { allowed: vec![ALLOWED.into()] }, false),
        WritePolicyCase::new(
            "redacted-owner",
            Some(OWNER),
            AccessPolicy::Redacted {
                visible_fields: vec![RedactableField::Content],
            },
            true,
        ),
        WritePolicyCase::new(
            "redacted-non-owner",
            Some(VIEWER),
            AccessPolicy::Redacted {
                visible_fields: vec![RedactableField::Content],
            },
            false,
        ),
        WritePolicyCase::new(
            "redacted-ownerless",
            None,
            AccessPolicy::Redacted {
                visible_fields: vec![RedactableField::Content],
            },
            false,
        ),
    ]
}

async fn assert_reembed_claim_contract<S>(store: &S, embedding_dimensions: usize, case: &str, base: DateTime<Utc>)
where
    S: MemoryStore,
{
    let policy_cases = write_policy_cases();
    let mut expected_authorized = BTreeSet::new();
    let mut expected_recovery = BTreeSet::new();
    let mut all_ids = Vec::with_capacity(policy_cases.len());
    for (offset, policy_case) in policy_cases.into_iter().enumerate() {
        let mut candidate = memory(MemorySpec {
            content: format!("reembed {} {case}", policy_case.name),
            tags: vec![format!("contract-{case}"), "reembed-policy".into()],
            source_agent: OWNER,
            scope: format!("contract/reembed/{case}"),
            origin: format!("contract/reembed-origin/{case}"),
            access_policy: policy_case.access_policy,
            created_at: time_after(base, i64::try_from(offset).unwrap()),
        });
        candidate.provenance.source_agent = policy_case.source_agent.map(str::to_owned);
        assert_eq!(candidate.has_write_access(OWNER), policy_case.authorized, "invalid {} fixture", policy_case.name);
        let id = store.store(&candidate, None).await.unwrap();
        if policy_case.authorized {
            assert!(expected_authorized.insert(id));
        } else {
            assert!(expected_recovery.insert(id));
        }
        all_ids.push(id);
    }

    let listed_ids = store.list_for_reembed(all_ids.len()).await.unwrap().into_iter().map(|(id, ..)| id).collect::<BTreeSet<_>>();
    assert_eq!(
        listed_ids,
        all_ids.iter().copied().collect(),
        "unrestricted listing must include every unembedded policy case"
    );

    let authorized_claims = store.claim_for_reembed_authorized(OWNER, expected_authorized.len()).await.unwrap();
    let actual_authorized = authorized_claims.iter().map(|claim| claim.id).collect::<BTreeSet<_>>();
    assert_eq!(actual_authorized, expected_authorized, "authorized claim policy must match Memory::has_write_access");

    let recovery_claims = store.claim_for_reembed(expected_recovery.len()).await.unwrap();
    let actual_recovery = recovery_claims.iter().map(|claim| claim.id).collect::<BTreeSet<_>>();
    assert_eq!(actual_recovery, expected_recovery, "process-owned recovery must include rows denied to the caller");

    let completed_claim = authorized_claims.first().unwrap().clone();
    for claim in authorized_claims.iter().chain(&recovery_claims) {
        store
            .set_embedding(&claim.id, &embedding(embedding_dimensions, 9.0_f32), claim.embedding_revision)
            .await
            .unwrap();
    }
    assert!(
        !store
            .release_embedding_claim(&completed_claim.id, completed_claim.embedding_revision, &completed_claim.claim_token)
            .await
            .unwrap()
    );
    assert_eq!(store.fetch_embeddings_for_ids(&all_ids).await.unwrap().len(), all_ids.len());
}

async fn assert_expired_cleanup_contract<S>(store: &S, case: &str, base: DateTime<Utc>)
where
    S: MemoryStore,
{
    let mut expired_cases = Vec::with_capacity(write_policy_cases().len());
    for (offset, policy_case) in write_policy_cases().into_iter().enumerate() {
        let offset = i64::try_from(offset).unwrap();
        let mut expired = memory(MemorySpec {
            content: format!("expired cleanup {} {case}", policy_case.name),
            tags: vec![format!("contract-{case}"), "expired-cleanup-policy".into()],
            source_agent: OWNER,
            scope: format!("contract/cleanup/{case}"),
            origin: format!("contract/cleanup-origin/{case}"),
            access_policy: policy_case.access_policy,
            created_at: time_after(base, 30_i64.saturating_add(offset)),
        });
        expired.provenance.source_agent = policy_case.source_agent.map(str::to_owned);
        expired.expires_at = Some(time_after(base, 60_i64.saturating_add(offset)));
        assert_eq!(expired.has_write_access(OWNER), policy_case.authorized, "invalid {} cleanup fixture", policy_case.name);
        let id = store.store(&expired, None).await.unwrap();
        expired_cases.push((id, policy_case.authorized));
    }

    let durable = memory(MemorySpec {
        content: format!("durable cleanup control {case}"),
        tags: vec![format!("contract-{case}"), "expired-cleanup-control".into()],
        source_agent: OWNER,
        scope: format!("contract/cleanup/{case}"),
        origin: format!("contract/cleanup-origin/{case}"),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 70),
    });
    let durable_id = store.store(&durable, None).await.unwrap();

    let authorized_audit = AuditDraft {
        action: AuditAction::Delete,
        caller_agent: Some(OWNER.into()),
        timestamp: time_after(base, 80),
        details: Some(json!({"mode": "authorized", "reason": "expired"})),
    };
    let expected_authorized = expired_cases.iter().filter(|case| case.1).count();
    assert_eq!(store.evict_expired(OWNER, &authorized_audit).await.unwrap(), u64::try_from(expected_authorized).unwrap());
    assert_authorized_cleanup_records(store, &expired_cases, &authorized_audit).await;
    assert_eq!(store.evict_expired(OWNER, &authorized_audit).await.unwrap(), 0);

    let all_audit = AuditDraft {
        action: AuditAction::Delete,
        caller_agent: Some(OWNER.into()),
        timestamp: time_after(base, 81),
        details: Some(json!({"mode": "all", "reason": "expired"})),
    };
    let expected_all = expired_cases.iter().filter(|case| !case.1).count();
    assert_eq!(store.evict_expired_all(OWNER, &all_audit).await.unwrap(), u64::try_from(expected_all).unwrap());
    assert_whole_store_cleanup_records(store, &expired_cases, &all_audit).await;
    assert!(store.get(&durable_id, Some(OWNER)).await.unwrap().is_some());
    assert!(store.get_tombstone(&durable_id).await.unwrap().is_none());
    assert_direct_whole_store_cleanup_contract(store, case, base).await;
}

async fn assert_authorized_cleanup_records<S>(store: &S, expired_cases: &[(MemoryId, bool)], audit: &AuditDraft)
where
    S: MemoryStore,
{
    for (id, authorized) in expired_cases {
        let tombstone = store.get_tombstone(id).await.unwrap();
        let history = store.query_audit_log(id, 10).await.unwrap();
        if *authorized {
            assert_cleanup_deletion_record(tombstone.as_ref(), &history, audit, "authorized cleanup");
        } else {
            assert!(tombstone.is_none(), "denied cleanup row must remain active");
            assert!(history.is_empty(), "denied cleanup row must not receive an audit entry");
        }
    }
}

async fn assert_whole_store_cleanup_records<S>(store: &S, expired_cases: &[(MemoryId, bool)], audit: &AuditDraft)
where
    S: MemoryStore,
{
    for (id, authorized) in expired_cases {
        if *authorized {
            continue;
        }
        let tombstone = store.get_tombstone(id).await.unwrap();
        let history = store.query_audit_log(id, 10).await.unwrap();
        assert_cleanup_deletion_record(tombstone.as_ref(), &history, audit, "whole-store cleanup");
    }
}

async fn assert_direct_whole_store_cleanup_contract<S>(store: &S, case: &str, base: DateTime<Utc>)
where
    S: MemoryStore,
{
    let direct_cases = [
        ("authorized", Some(OWNER), AccessPolicy::Public, true),
        (
            "denied",
            Some(VIEWER),
            AccessPolicy::Redacted {
                visible_fields: vec![RedactableField::Content],
            },
            false,
        ),
    ];
    let mut ids = Vec::with_capacity(direct_cases.len());
    for (offset, (name, source_agent, access_policy, authorized)) in direct_cases.into_iter().enumerate() {
        let offset = i64::try_from(offset).unwrap();
        let mut expired = memory(MemorySpec {
            content: format!("direct whole-store cleanup {name} {case}"),
            tags: vec![format!("contract-{case}"), "direct-expired-cleanup".into()],
            source_agent: OWNER,
            scope: format!("contract/direct-cleanup/{case}"),
            origin: format!("contract/direct-cleanup-origin/{case}"),
            access_policy,
            created_at: time_after(base, 90_i64.saturating_add(offset)),
        });
        expired.provenance.source_agent = source_agent.map(str::to_owned);
        expired.expires_at = Some(time_after(base, 100_i64.saturating_add(offset)));
        assert_eq!(expired.has_write_access(OWNER), authorized, "invalid direct whole-store {name} fixture");
        ids.push(store.store(&expired, None).await.unwrap());
    }

    let audit = AuditDraft {
        action: AuditAction::Delete,
        caller_agent: Some(OWNER.into()),
        timestamp: time_after(base, 110),
        details: Some(json!({"mode": "all", "reason": "expired"})),
    };
    assert_eq!(store.evict_expired_all(OWNER, &audit).await.unwrap(), u64::try_from(ids.len()).unwrap());
    for id in ids {
        let tombstone = store.get_tombstone(&id).await.unwrap();
        let history = store.query_audit_log(&id, 10).await.unwrap();
        assert_cleanup_deletion_record(tombstone.as_ref(), &history, &audit, "direct whole-store cleanup");
    }
}

fn assert_cleanup_deletion_record(tombstone: Option<&crate::types::MemoryTombstone>, history: &[crate::types::AuditEntry], audit: &AuditDraft, operation: &str) {
    assert_eq!(
        tombstone.and_then(|row| row.deleted_by_principal.as_deref()),
        Some(OWNER),
        "{operation} must create an attributed tombstone"
    );
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].action, AuditAction::Delete);
    assert_eq!(history[0].caller_agent.as_deref(), Some(OWNER));
    assert_eq!(history[0].details.as_ref(), audit.details.as_ref());
}

/// Exercise invalid vector values consistently across every backend entry point.
pub(crate) async fn assert_non_finite_embeddings_rejected<S>(store: &S, embedding_dimensions: usize)
where
    S: MemoryStore,
{
    assert!(embedding_dimensions > 0, "conformance requires positive embedding dimensions");

    let case = MemoryId::new().to_string();
    let scope = format!("contract/invalid/{case}");
    let origin = format!("contract/invalid-origin/{case}");
    let ctx = QueryContext { principal: Some(OWNER.into()) };

    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut bad = embedding(embedding_dimensions, 0.0_f32);
        bad[0] = value;
        let memory = memory(MemorySpec {
            content: format!("invalid embedding {value} {case}"),
            tags: vec![format!("invalid-{case}")],
            source_agent: OWNER,
            scope: scope.clone(),
            origin: origin.clone(),
            access_policy: AccessPolicy::Public,
            created_at: fixed_time(),
        });

        let err = store.store(&memory, Some(&bad)).await.unwrap_err();
        assert_non_finite_error(err);
        assert!(store.get(&memory.id, Some(OWNER)).await.unwrap().is_none());

        let err = store.search_by_embedding(&bad, 1, &MemoryFilter::default(), &ctx, None).await.unwrap_err();
        assert_non_finite_error(err);

        let err = store.find_embedding_neighbors(&bad, 1.0_f64, 1).await.unwrap_err();
        assert_non_finite_error(err);
    }

    let unembedded = memory(MemorySpec {
        content: format!("invalid set embedding {case}"),
        tags: vec![format!("invalid-set-{case}")],
        source_agent: OWNER,
        scope,
        origin,
        access_policy: AccessPolicy::Public,
        created_at: time_after(fixed_time(), 1),
    });
    let unembedded_id = store.store(&unembedded, None).await.unwrap();
    let (_, revision) = store.get_for_reembed(&unembedded_id, OWNER).await.unwrap().unwrap();
    let mut bad = embedding(embedding_dimensions, 0.0_f32);
    bad[0] = f32::NAN;
    let err = store.set_embedding(&unembedded_id, &bad, revision).await.unwrap_err();
    assert_non_finite_error(err);
    assert!(!store.fetch_embeddings_for_ids(&[unembedded_id]).await.unwrap().contains_key(&unembedded_id));
}

fn fixed_time() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 5, 31, 12, 0, 0).single().unwrap()
}

fn time_after(base: DateTime<Utc>, seconds: i64) -> DateTime<Utc> {
    base.checked_add_signed(Duration::seconds(seconds)).unwrap()
}

fn memory(spec: MemorySpec) -> Memory {
    Memory {
        id: MemoryId::new(),
        content: spec.content,
        tags: spec.tags,
        provenance: Provenance {
            source_agent: Some(spec.source_agent.into()),
            source_conversation: Some(spec.scope),
            origin_conversation: Some(spec.origin),
            source_user: None,
        },
        access_policy: spec.access_policy,
        created_at: spec.created_at,
        updated_at: spec.created_at,
        record_revision: 0_i64,
        expires_at: None,
        has_embedding: false,
        memory_type: MemoryType::Semantic,
        importance: Importance::new(0.75_f64),
        confidence: Confidence::new(0.9_f64),
        impression_count: 0,
        last_impressed_at: None,
        superseded_by: None,
        activity_mass: 0.0_f64,
        last_used_at: None,
        entities: Vec::new(),
        was_redacted: false,
    }
}

fn embedding(dimensions: usize, first_value: f32) -> Vec<f32> {
    let mut values = vec![0.0_f32; dimensions];
    values[0] = first_value;
    values
}

fn case_filter(case_tag: &str) -> MemoryFilter {
    MemoryFilter {
        tags: Some(vec![case_tag.into()]),
        ..MemoryFilter::default()
    }
}

fn ids(memories: &[Memory]) -> Vec<MemoryId> {
    memories.iter().map(|memory| memory.id).collect()
}

fn search_ids(results: &[SearchResult]) -> Vec<MemoryId> {
    results.iter().map(|result| result.memory.id).collect()
}

fn assert_search_contains(results: &[SearchResult], id: MemoryId) {
    let ids = search_ids(results);
    assert!(ids.contains(&id), "expected search results to contain {id}, got {ids:?}");
}

fn assert_non_finite_error(err: StoreError) {
    let actual = format!("{err:?}");
    assert!(matches!(&err, StoreError::Conflict(_)), "expected non-finite embedding conflict, got {actual}");
    let StoreError::Conflict(message) = err else { return };
    assert!(message.contains("non-finite"), "unexpected conflict: {message}");
}
