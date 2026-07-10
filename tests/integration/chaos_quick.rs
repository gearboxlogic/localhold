//! Scripted chaos tests exercising fault injection against `RecallEngine`.

use std::sync::Arc;

use localhold::{
    config::{LimitsConfig, SearchConfig},
    embedding::NoopEmbedding,
    engine::RecallEngine,
    error::EngineError,
    store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
    types::{AccessPolicy, Memory, MemoryFilter, Provenance, QueryContext, WriteOutcome},
};

use super::{
    fault_injection::{
        ChaosEmbedding, ChaosStore, FaultPlan, chaos_store_always_fail_search, chaos_store_always_fail_store, chaos_store_countdown_get, chaos_store_countdown_store,
        chaos_store_passthrough,
    },
    helpers::DeterministicEmbedding,
};

fn make_memory(content: &str) -> Memory {
    Memory::new_for_test(
        content.to_owned(),
        vec!["test".to_owned()],
        Provenance::new_for_test(Some("test-agent".to_owned()), None, None),
        AccessPolicy::Public,
    )
}

fn make_engine(store: ChaosStore<SqliteStore>, embedding: Arc<dyn localhold::embedding::EmbeddingProvider>) -> RecallEngine<ChaosStore<SqliteStore>> {
    RecallEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default())
}

// ---------------------------------------------------------------------------
// 1. store_fails_returns_error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_store_fails_returns_error() {
    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_always_fail_store(inner);
    let engine = make_engine(store, Arc::new(NoopEmbedding::new()));

    let result = engine.store_memory(make_memory("should fail"), None).await;
    assert!(result.is_err(), "store should return an error");
    assert!(matches!(result.unwrap_err(), EngineError::Store(_)), "error should be a Store variant");
}

// ---------------------------------------------------------------------------
// 2. get_after_store_failure_returns_none
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_get_after_store_failure_returns_none() {
    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_always_fail_store(inner);
    let engine = make_engine(store, Arc::new(NoopEmbedding::new()));

    let memory = make_memory("never stored");
    let id = memory.id;
    let _err = engine.store_memory(memory, None).await;

    let result = engine.get_memory(&id, None).await.unwrap();
    assert!(result.is_none(), "get should return None for unstored memory");
}

// ---------------------------------------------------------------------------
// 3. search_fails_gracefully
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_search_fails_gracefully() {
    let inner = SqliteStore::in_memory().unwrap();

    // Store a memory directly via the raw inner store before wrapping
    let _id = inner.store(&make_memory("searchable content"), None).await.unwrap();

    let store = chaos_store_always_fail_search(inner);
    let engine = make_engine(store, Arc::new(NoopEmbedding::new()));

    // search_by_text should fail because the chaos plan intercepts it.
    let result = engine
        .store()
        .search_by_text("searchable", 10_usize, &MemoryFilter::default(), &QueryContext::default())
        .await;

    assert!(result.is_err(), "search should return an error when store search fails");
}

// ---------------------------------------------------------------------------
// 4. delete_after_store_failure_is_idempotent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_delete_after_store_failure_is_idempotent() {
    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_always_fail_store(inner);
    let engine = make_engine(store, Arc::new(NoopEmbedding::new()));

    let memory = make_memory("will not be stored");
    let id = memory.id;
    let _err = engine.store_memory(memory, None).await;

    // Delete of nonexistent ID should not error
    let outcome = engine.delete_memory(&id, "test-agent").await.unwrap();
    assert_eq!(outcome, WriteOutcome::NotFound, "deleting a never-stored memory should return NotFound");
}

// ---------------------------------------------------------------------------
// 5. embedding_failure_still_stores_memory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_embedding_failure_still_stores_memory() {
    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_passthrough(inner);

    let chaos_embed = ChaosEmbedding::new(Arc::new(DeterministicEmbedding), FaultPlan::Always);

    let engine = RecallEngine::new(store, Arc::new(chaos_embed), LimitsConfig::default(), SearchConfig::default());

    let memory = make_memory("should still store");
    let id = engine.store_memory(memory, None).await.unwrap();

    // Wait briefly for background embed task to fail
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let retrieved = engine.get_memory(&id, None).await.unwrap();
    assert!(retrieved.is_some(), "memory should be retrievable despite embedding failure");
    assert_eq!(retrieved.unwrap().content, "should still store");
}

// ---------------------------------------------------------------------------
// 6. countdown_store_recovers_after_n_failures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_countdown_store_recovers_after_n_failures() {
    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_countdown_store(inner, 2_usize);
    let engine = make_engine(store, Arc::new(NoopEmbedding::new()));

    // First two stores should fail
    let r1 = engine.store_memory(make_memory("attempt 1"), None).await;
    assert!(r1.is_err(), "first store should fail");

    let r2 = engine.store_memory(make_memory("attempt 2"), None).await;
    assert!(r2.is_err(), "second store should fail");

    // Third store should succeed
    let r3 = engine.store_memory(make_memory("attempt 3"), None).await;
    assert!(r3.is_ok(), "third store should succeed after countdown expires");

    let id = r3.unwrap();
    let retrieved = engine.get_memory(&id, None).await.unwrap();
    assert!(retrieved.is_some(), "stored memory should be retrievable");
    assert_eq!(retrieved.unwrap().content, "attempt 3");
}

// ---------------------------------------------------------------------------
// 7. concurrent_store_with_intermittent_failures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_concurrent_store_with_intermittent_failures() {
    let inner = SqliteStore::in_memory().unwrap();
    // First 3 of 10 concurrent stores will fail
    let store = chaos_store_countdown_store(inner, 3_usize);
    let engine = make_engine(store, Arc::new(NoopEmbedding::new()));

    let mut handles = Vec::with_capacity(10_usize);
    for i in 0_i32..10_i32 {
        let eng = engine.clone();
        handles.push(tokio::spawn(async move { eng.store_memory(make_memory(&format!("concurrent-{i}")), None).await }));
    }

    let mut successes = 0_usize;
    let mut failures = 0_usize;
    for handle in handles {
        match handle.await.unwrap() {
            Ok(_id) => successes += 1_usize,
            Err(_) => failures += 1_usize,
        }
    }

    assert_eq!(failures, 3_usize, "exactly 3 stores should fail due to countdown");
    assert_eq!(successes, 7_usize, "exactly 7 stores should succeed");

    // Verify all successful stores are retrievable
    let listed = engine.list_memories(MemoryFilter::default(), QueryContext::default()).await.unwrap();
    assert_eq!(listed.len(), 7_usize, "should have exactly 7 memories in the store");
}

// ---------------------------------------------------------------------------
// 8. get_failure_does_not_corrupt_store
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_get_failure_does_not_corrupt_store() {
    let inner = SqliteStore::in_memory().unwrap();
    // Get fails once, then succeeds
    let store = chaos_store_countdown_get(inner, 1_usize);
    let engine = make_engine(store, Arc::new(NoopEmbedding::new()));

    // Store a memory successfully
    let memory = make_memory("persistent data");
    let id = engine.store_memory(memory, None).await.unwrap();

    // First get fails
    let r1 = engine.get_memory(&id, None).await;
    assert!(r1.is_err(), "first get should fail due to countdown");

    // Second get succeeds and returns correct data
    let r2 = engine.get_memory(&id, None).await.unwrap();
    assert!(r2.is_some(), "second get should succeed");
    assert_eq!(r2.unwrap().content, "persistent data", "data should be intact after a failed get");
}
