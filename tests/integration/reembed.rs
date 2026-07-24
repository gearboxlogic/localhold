use std::{sync::Arc, time::Duration};

use localhold::{
    config::LimitsConfig,
    server::params::{AdminListResponse, ReadResponse, ReembedResponse, RememberResponse, UpdateResponse},
    store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
    types::{AccessPolicy, Memory, Provenance},
};
use serde_json::json;

use super::helpers::{
    ToggleableEmbedding, assert_invalid_params_contains, await_embeddings, call_tool, call_tool_error, setup_embedding_server, setup_noop_server, setup_noop_server_with_limits,
    setup_server_with, setup_server_with_limits, setup_server_with_store,
};

#[tokio::test]
async fn admin_reembed_bulk_unembedded() {
    let (toggleable, flag) = ToggleableEmbedding::new(false);
    let (client, server) = setup_server_with(Arc::new(toggleable)).await;

    for i in 0_i32..3_i32 {
        let _stored: RememberResponse = call_tool(&client, "remember", json!({"content": format!("bulk-{i}")})).await;
    }

    await_embeddings(&server, Duration::from_secs(5)).await;

    let listed: AdminListResponse = call_tool(&client, "admin_list", json!({"has_embedding": false})).await;
    assert_eq!(listed.count, 3);

    flag.store(true, std::sync::atomic::Ordering::Relaxed);

    let resp: ReembedResponse = call_tool(&client, "admin_reembed", json!({})).await;
    assert_eq!(resp.queued, 3);

    await_embeddings(&server, Duration::from_secs(5)).await;

    let listed: AdminListResponse = call_tool(&client, "admin_list", json!({"has_embedding": true})).await;
    assert_eq!(listed.count, 3, "all 3 should now have embeddings");
}

#[tokio::test]
async fn admin_reembed_bulk_applies_write_authorization_before_limit() {
    let store = SqliteStore::in_memory().unwrap();
    let base = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH;

    let mut inaccessible = Memory::new_for_test(
        "inaccessible reembed content".into(),
        Vec::new(),
        Provenance::new_for_test(Some("other-agent".into()), None, None),
        AccessPolicy::Restricted {
            allowed: vec!["other-agent".into()],
        },
    );
    inaccessible.created_at = base;
    inaccessible.updated_at = base;
    let inaccessible_id = store.store(&inaccessible, None).await.unwrap();

    let mut authorized = Memory::new_for_test(
        "authorized reembed content".into(),
        Vec::new(),
        Provenance::new_for_test(Some("stdio".into()), None, None),
        AccessPolicy::Public,
    );
    let later = base.checked_add_signed(chrono::Duration::seconds(1)).unwrap();
    authorized.created_at = later;
    authorized.updated_at = later;
    let authorized_id = store.store(&authorized, None).await.unwrap();

    let (embedding, _enabled) = ToggleableEmbedding::new(true);
    let (client, server) = setup_server_with_store(store.clone(), Arc::new(embedding)).await;

    let response: ReembedResponse = call_tool(&client, "admin_reembed", json!({"limit": 1_i32})).await;
    assert_eq!(response.queued, 1, "the authorized row should fill the limit past the older denied row");
    await_embeddings(&server, Duration::from_secs(5)).await;

    let authorized_after = store.get(&authorized_id, Some("stdio")).await.unwrap().unwrap();
    assert!(authorized_after.has_embedding);
    let inaccessible_after = store.get(&inaccessible_id, Some("other-agent")).await.unwrap().unwrap();
    assert!(!inaccessible_after.has_embedding, "content outside the caller's write authority must not be embedded");

    let retry: ReembedResponse = call_tool(&client, "admin_reembed", json!({"limit": 1_i32})).await;
    assert_eq!(retry.queued, 0, "inaccessible rows should remain invisible to the bulk maintenance caller");

    server.shutdown().await;
}

#[tokio::test]
async fn admin_reembed_noop_provider_returns_error() {
    let client = setup_noop_server().await;

    assert_invalid_params_contains(&client, "admin_reembed", json!({}), "disabled").await;
}

#[tokio::test]
async fn admin_reembed_rejects_bulk_limit_before_provider_health_check() {
    let mut limits = LimitsConfig::default();
    limits.max_reembed_limit = 1;
    let (client, _server) = setup_noop_server_with_limits(limits).await;

    let err = call_tool_error(&client, "admin_reembed", json!({"limit": 2_i32})).await;

    assert!(err.contains("exceeds maximum batch size of 1"), "expected limit cap error, got: {err}");
    assert!(!err.contains("disabled"), "bulk limit must fail before embedding health check: {err}");
}

#[tokio::test]
async fn admin_reembed_rejects_zero_bulk_limit() {
    let (client, _server) = setup_embedding_server().await;

    let err = call_tool_error(&client, "admin_reembed", json!({"limit": 0_i32})).await;

    assert!(err.contains("cannot be empty"), "expected zero-limit error, got: {err}");
}

#[tokio::test]
async fn admin_reembed_single_not_found() {
    let (client, server) = setup_embedding_server().await;

    let resp = call_tool_error(&client, "admin_reembed", json!({"id": "01ARZ3NDEKTSV4RRFFQ69G5FAV"})).await;
    assert!(resp.contains("not found"), "expected not found, got: {resp}");

    server.shutdown().await;
}

#[tokio::test]
async fn admin_reembed_bulk_respects_limit() {
    let (toggleable, flag) = ToggleableEmbedding::new(false);
    let (client, server) = setup_server_with(Arc::new(toggleable)).await;

    for i in 0_i32..5_i32 {
        let _stored: RememberResponse = call_tool(&client, "remember", json!({"content": format!("limit-{i}")})).await;
    }

    await_embeddings(&server, Duration::from_secs(5)).await;

    flag.store(true, std::sync::atomic::Ordering::Relaxed);

    let resp: ReembedResponse = call_tool(&client, "admin_reembed", json!({"limit": 2_i32})).await;
    assert_eq!(resp.queued, 2);

    await_embeddings(&server, Duration::from_secs(5)).await;

    let embedded: AdminListResponse = call_tool(&client, "admin_list", json!({"has_embedding": true})).await;
    assert_eq!(embedded.count, 2, "only 2 should be embedded");

    let unembedded: AdminListResponse = call_tool(&client, "admin_list", json!({"has_embedding": false})).await;
    assert_eq!(unembedded.count, 3, "3 should still lack embeddings");
}

#[tokio::test]
async fn admin_reembed_bulk_default_uses_configured_limit() {
    let mut limits = LimitsConfig::default();
    limits.max_reembed_limit = 1;
    let (toggleable, flag) = ToggleableEmbedding::new(false);
    let (client, server) = setup_server_with_limits(Arc::new(toggleable), limits).await;

    for i in 0_i32..3_i32 {
        let _stored: RememberResponse = call_tool(&client, "remember", json!({"content": format!("configured-default-{i}")})).await;
    }
    await_embeddings(&server, Duration::from_secs(5)).await;

    flag.store(true, std::sync::atomic::Ordering::Relaxed);

    let resp: ReembedResponse = call_tool(&client, "admin_reembed", json!({})).await;
    assert_eq!(resp.queued, 1);
}

#[tokio::test]
async fn admin_list_has_embedding_filter_integration() {
    let (client, server) = setup_embedding_server().await;

    let _stored: RememberResponse = call_tool(&client, "remember", json!({"content": "will embed"})).await;

    await_embeddings(&server, Duration::from_secs(5)).await;

    let with: AdminListResponse = call_tool(&client, "admin_list", json!({"has_embedding": true})).await;
    assert_eq!(with.count, 1);

    let without: AdminListResponse = call_tool(&client, "admin_list", json!({"has_embedding": false})).await;
    assert_eq!(without.count, 0);

    server.shutdown().await;
}

#[tokio::test]
async fn admin_reembed_concurrent_safety() {
    let (client, server) = setup_embedding_server().await;

    let stored: RememberResponse = call_tool(&client, "remember", json!({"content": "concurrent test"})).await;
    await_embeddings(&server, Duration::from_secs(5)).await;

    let _updated: UpdateResponse = call_tool(&client, "revise", json!({"id": stored.id, "content": "concurrent updated"})).await;

    let client = Arc::new(client);
    let client2 = Arc::clone(&client);
    let id = stored.id;
    let handle1 = tokio::spawn(async move {
        let _queued: ReembedResponse = call_tool(&client2, "admin_reembed", json!({"id": id})).await;
    });
    let client3 = Arc::clone(&client);
    let id2 = stored.id;
    let handle2 = tokio::spawn(async move {
        let _queued: ReembedResponse = call_tool(&client3, "admin_reembed", json!({"id": id2})).await;
    });

    handle1.await.unwrap();
    handle2.await.unwrap();

    await_embeddings(&server, Duration::from_secs(5)).await;

    let read: ReadResponse = call_tool(&client, "read", json!({"id": stored.id})).await;
    assert_eq!(read.memory.content, "concurrent updated");

    server.shutdown().await;
}
