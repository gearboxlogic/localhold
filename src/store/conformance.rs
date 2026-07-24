//! Shared store contract checks for every `MemoryStore` backend.

use chrono::{DateTime, Duration, TimeZone as _, Utc};
use serde_json::json;

use super::{MemoryStore, MemoryWithEmbedding};
use crate::{
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

    let retrieved = store.get(&primary_id, None).await.unwrap().unwrap();
    assert_eq!(retrieved.content, primary.content);
    assert_eq!(retrieved.entities, primary.entities);

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

    let scoped_embeddings = store.list_with_embeddings(Some(std::slice::from_ref(&scope)), 10).await.unwrap();
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

    let inaccessible_unembedded = memory(MemorySpec {
        content: format!("inaccessible reembed {case}"),
        tags: vec![case_tag.clone(), "reembed-denied".into()],
        source_agent: VIEWER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Restricted { allowed: vec![VIEWER.into()] },
        created_at: time_after(base, 10),
    });
    let inaccessible_unembedded_id = store.store(&inaccessible_unembedded, None).await.unwrap();
    let unembedded = memory(MemorySpec {
        content: format!("needs reembed {case}"),
        tags: vec![case_tag.clone(), "reembed".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 11),
    });
    let unembedded_id = store.store(&unembedded, None).await.unwrap();
    let second_unembedded = memory(MemorySpec {
        content: format!("also needs reembed {case}"),
        tags: vec![case_tag.clone(), "reembed".into()],
        source_agent: OWNER,
        scope: scope.clone(),
        origin: origin.clone(),
        access_policy: AccessPolicy::Public,
        created_at: time_after(base, 12),
    });
    let second_unembedded_id = store.store(&second_unembedded, None).await.unwrap();
    assert!(
        store
            .list_for_reembed(50)
            .await
            .unwrap()
            .iter()
            .any(|(id, content, _)| *id == unembedded_id && content == &unembedded.content)
    );
    let claims = store.claim_for_reembed_authorized(OWNER, 2).await.unwrap();
    assert_eq!(claims.len(), 2, "authorized batch limit should be filled past inaccessible older rows");
    let claim = claims.iter().find(|claim| claim.id == unembedded_id).unwrap();
    let second_claim = claims.iter().find(|claim| claim.id == second_unembedded_id).unwrap();
    assert_eq!(claim.claim_token, second_claim.claim_token, "one batch must share one durable claim token");
    assert_eq!(
        store.get_for_reembed(&unembedded_id, OWNER).await.unwrap(),
        Some((unembedded.content.clone(), claim.embedding_revision))
    );
    store
        .set_embedding(&unembedded_id, &embedding(embedding_dimensions, 9.0_f32), claim.embedding_revision)
        .await
        .unwrap();
    store
        .set_embedding(&second_unembedded_id, &embedding(embedding_dimensions, 9.25_f32), second_claim.embedding_revision)
        .await
        .unwrap();
    assert!(!store.release_embedding_claim(&unembedded_id, claim.embedding_revision, &claim.claim_token).await.unwrap());
    let reembedded = store.fetch_embeddings_for_ids(&[unembedded_id, second_unembedded_id]).await.unwrap();
    assert!(reembedded.contains_key(&unembedded_id));
    assert!(reembedded.contains_key(&second_unembedded_id));
    assert!(
        store.claim_for_reembed_authorized(OWNER, 50).await.unwrap().is_empty(),
        "inaccessible rows must remain unclaimed for the requesting principal"
    );
    let viewer_claims = store.claim_for_reembed_authorized(VIEWER, 1).await.unwrap();
    assert_eq!(viewer_claims.len(), 1);
    assert_eq!(viewer_claims[0].id, inaccessible_unembedded_id);
    store
        .set_embedding(&inaccessible_unembedded_id, &embedding(embedding_dimensions, 9.5_f32), viewer_claims[0].embedding_revision)
        .await
        .unwrap();

    let update = MemoryUpdate {
        content: Some(format!("updated content {case}")),
        ..MemoryUpdate::default()
    };
    let updated = store.update_authorized(&primary_id, &update, OWNER).await.unwrap();
    assert_eq!(updated.outcome, WriteOutcome::Applied);
    assert!(updated.reembed_revision.is_some());

    let use_now = time_after(base, 13);
    let use_outcome = store.record_memory_use(&[primary_id, MemoryId::new()], OWNER, 1.0_f64, use_now, 24.0_f64).await.unwrap();
    assert_eq!(use_outcome.recorded, 1_u64);
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
    let content_audit = AuditDraft {
        action: AuditAction::Update,
        caller_agent: Some(OWNER.into()),
        timestamp: time_after(base, 14),
        details: Some(json!({ "case": case_tag, "old_content_hash": "stale" })),
    };
    let content_update = MemoryUpdate {
        content: Some(content_after_audit_update.clone()),
        ..MemoryUpdate::default()
    };
    let content_outcome = store.update_authorized_audited(&audited_id, &content_update, OWNER, &content_audit).await.unwrap();
    assert_eq!(content_outcome.outcome, WriteOutcome::Applied);
    let content_history = store.query_audit_log(&audited_id, 10).await.unwrap();
    let content_audit_entry = content_history.iter().find(|entry| entry.timestamp == content_audit.timestamp).unwrap();
    let content_details = content_audit_entry.details.as_ref().and_then(serde_json::Value::as_object).unwrap();
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
    let reassigned = store.reassign_scope(&from_scope, &to_scope, None, OWNER).await.unwrap();
    assert_eq!(reassigned.applied_ids, vec![movable_id]);
    let moved = store.get(&movable_id, Some(OWNER)).await.unwrap().unwrap();
    assert_eq!(moved.provenance.source_conversation.as_deref(), Some(to_scope.as_str()));
    assert!(
        moved.record_revision > opened_movable.record_revision,
        "scope reassignment must advance the optimistic revision"
    );
    assert_eq!(moved.updated_at, opened_movable.updated_at, "scope reassignment must preserve content freshness");

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
