//! Seeded probabilistic chaos tests with reproducible failure patterns.

use std::sync::Arc;

use localhold::{
    config::{LimitsConfig, SearchConfig},
    embedding::NoopEmbedding,
    engine::RecallEngine,
    store::{MemoryReader as _, SqliteStore},
    types::{AccessPolicy, Memory, MemoryFilter, Provenance, QueryContext},
};

use super::{
    fault_injection::{
        ChaosEmbedding, ChaosStore, FaultPlan, chaos_store_multi_probabilistic, chaos_store_passthrough, chaos_store_probabilistic_search, chaos_store_probabilistic_store,
    },
    helpers::DeterministicEmbedding,
};

/// Fixed seed for reproducible probabilistic tests.
const SEED: u64 = 42;

fn make_memory(content: &str) -> Memory {
    Memory::new_for_test(
        content.to_owned(),
        vec!["chaos".to_owned()],
        Provenance::new_for_test(Some("chaos-agent".to_owned()), None, None),
        AccessPolicy::Public,
    )
}

fn make_engine(store: ChaosStore<SqliteStore>, embedding: Arc<dyn localhold::embedding::EmbeddingProvider>) -> RecallEngine<ChaosStore<SqliteStore>> {
    RecallEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default())
}

// ---------------------------------------------------------------------------
// 1. probabilistic_store_failures_no_data_loss
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_probabilistic_store_failures_no_data_loss() {
    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_probabilistic_store(inner, 0.5_f64, SEED);
    let engine = make_engine(store, Arc::new(NoopEmbedding::new()));

    let mut stored_ids = Vec::new();
    let total_attempts = 20_usize;

    for i in 0_usize..total_attempts {
        let memory = make_memory(&format!("prob-store-{i}"));
        if let Ok(id) = engine.store_memory(memory, None).await {
            stored_ids.push(id);
        }
    }

    // At least some should succeed (very unlikely all 20 fail at 50%)
    assert!(!stored_ids.is_empty(), "at least one store should succeed with 50% failure rate over 20 attempts");

    // Every stored memory should be retrievable
    for id in &stored_ids {
        let mem = engine.get_memory(id, None).await.unwrap();
        assert!(mem.is_some(), "stored memory {id} should be retrievable");
    }

    // Count should match stored IDs
    let listed = engine.list_memories(MemoryFilter::default(), QueryContext::default()).await.unwrap();
    assert_eq!(listed.len(), stored_ids.len(), "listed count should match successfully stored count");
}

// ---------------------------------------------------------------------------
// 2. probabilistic_search_degrades_gracefully
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_probabilistic_search_degrades_gracefully() {
    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_probabilistic_search(inner, 0.3_f64, SEED);
    let engine = make_engine(store, Arc::new(NoopEmbedding::new()));

    // Store some memories first (store plan is None, always succeeds)
    for i in 0_i32..5_i32 {
        let _id = engine.store_memory(make_memory(&format!("searchable-{i}")), None).await.unwrap();
    }

    let mut successes = 0_usize;
    let mut failures = 0_usize;
    let total_searches = 20_usize;

    // Use the store's search_by_text directly since engine.search_memories is pub(crate)
    for _ in 0_usize..total_searches {
        let result = engine
            .store()
            .search_by_text("searchable", 10_usize, &MemoryFilter::default(), &QueryContext::default())
            .await;
        match result {
            Ok(_) => successes += 1_usize,
            Err(_) => failures += 1_usize,
        }
    }

    // Should have a mix of successes and failures
    assert!(successes > 0_usize, "at least some searches should succeed");
    assert!(failures > 0_usize, "at least some searches should fail with 30% failure rate over 20 attempts");
}

// ---------------------------------------------------------------------------
// 3. mixed_probabilistic_faults
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_mixed_probabilistic_faults() {
    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_multi_probabilistic(inner, 0.2_f64, 0.2_f64, 0.2_f64, SEED);
    let engine = make_engine(store, Arc::new(NoopEmbedding::new()));

    let mut stored_ids = Vec::new();

    // Mixed workload: store, get, search
    for i in 0_usize..15_usize {
        let memory = make_memory(&format!("mixed-{i}"));
        if let Ok(id) = engine.store_memory(memory, None).await {
            stored_ids.push(id);
        }
    }

    assert!(!stored_ids.is_empty(), "at least some stores should succeed with 20% failure rate");

    // Try to get each stored memory
    let mut get_successes = 0_usize;
    for id in &stored_ids {
        if engine.get_memory(id, None).await.is_ok() {
            get_successes += 1_usize;
        }
    }
    assert!(get_successes > 0_usize, "at least some gets should succeed");

    // Try a few searches via store trait
    let mut search_successes = 0_usize;
    for _ in 0_usize..10_usize {
        if engine
            .store()
            .search_by_text("mixed", 10_usize, &MemoryFilter::default(), &QueryContext::default())
            .await
            .is_ok()
        {
            search_successes += 1_usize;
        }
    }
    assert!(search_successes > 0_usize, "at least some searches should succeed");
}

// ---------------------------------------------------------------------------
// 4. probabilistic_embedding_failures
// ---------------------------------------------------------------------------

#[tokio::test]
async fn chaos_probabilistic_embedding_failures() {
    use rand::SeedableRng as _;

    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_passthrough(inner);

    let chaos_embed = ChaosEmbedding::new(Arc::new(DeterministicEmbedding), FaultPlan::Probabilistic {
        probability: 0.5_f64,
        rng: parking_lot::Mutex::new(rand::rngs::StdRng::seed_from_u64(SEED)),
    });

    let engine = RecallEngine::new(store, Arc::new(chaos_embed), LimitsConfig::default(), SearchConfig::default());

    let mut stored_ids = Vec::new();
    for i in 0_usize..10_usize {
        let memory = make_memory(&format!("embed-chaos-{i}"));
        let id = engine.store_memory(memory, None).await.unwrap();
        stored_ids.push(id);
    }

    // Wait for background embedding tasks to complete (some fail, some succeed)
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // All memories should be stored regardless of embedding outcome
    for id in &stored_ids {
        let mem = engine.get_memory(id, None).await.unwrap();
        assert!(mem.is_some(), "memory {id} should exist regardless of embedding success");
    }

    assert_eq!(stored_ids.len(), 10_usize, "all 10 stores should succeed");
}
