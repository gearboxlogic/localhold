//! Extended chaos tests -- `#[ignore]` stress tests for CI-only runs.

use std::sync::Arc;

use localhold::{
    config::{LimitsConfig, SearchConfig},
    embedding::NoopEmbedding,
    engine::RecallEngine,
    store::SqliteStore,
    types::{AccessPolicy, Memory, MemoryFilter, Provenance, QueryContext},
};

use super::{
    fault_injection::{ChaosEmbedding, ChaosStore, FaultPlan, chaos_store_countdown_store, chaos_store_passthrough, chaos_store_probabilistic_store},
    helpers::DeterministicEmbedding,
};

fn make_memory(content: &str) -> Memory {
    Memory::new_for_test(
        content.to_owned(),
        vec!["stress".to_owned()],
        Provenance::new_for_test(Some("stress-agent".to_owned()), None, None),
        AccessPolicy::Public,
    )
}

fn make_engine(store: ChaosStore<SqliteStore>, embedding: Arc<dyn localhold::embedding::EmbeddingProvider>) -> RecallEngine<ChaosStore<SqliteStore>> {
    let mut limits = LimitsConfig::default();
    limits.max_list_limit = 2000_usize;
    RecallEngine::new(store, embedding, limits, SearchConfig::default())
}

// ---------------------------------------------------------------------------
// 1. sustained_store_under_chaos
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "extended stress test -- run with --include-ignored"]
async fn chaos_sustained_store_under_chaos() {
    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_probabilistic_store(inner, 0.1_f64, 12345_u64);
    let engine = make_engine(store, Arc::new(NoopEmbedding::new()));

    let total_ops = 1000_usize;
    let mut successes = 0_usize;
    let mut failures = 0_usize;

    for i in 0_usize..total_ops {
        match engine.store_memory(make_memory(&format!("sustained-{i}")), None).await {
            Ok(_) => successes += 1_usize,
            Err(_) => failures += 1_usize,
        }
    }

    // With 10% failure rate over 1000 ops, we should see ~900 successes
    assert!(successes > 800_usize, "expected >800 successes out of 1000, got {successes}");
    assert!(failures > 50_usize, "expected >50 failures out of 1000, got {failures}");

    // Verify all successful stores are retrievable
    let mut filter = MemoryFilter::default();
    filter.limit = Some(total_ops);
    let listed = engine.list_memories(filter, QueryContext::default()).await.unwrap();
    assert_eq!(listed.len(), successes, "listed count should match successful store count");
}

// ---------------------------------------------------------------------------
// 2. recovery_after_total_outage
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "extended stress test -- run with --include-ignored"]
async fn chaos_recovery_after_total_outage() {
    let inner = SqliteStore::in_memory().unwrap();
    // Store operations fail for the first 100 calls; get/search/delete pass through
    let store = chaos_store_countdown_store(inner, 100_usize);
    let engine = make_engine(store, Arc::new(NoopEmbedding::new()));

    // Outage phase: all stores fail
    let mut outage_failures = 0_usize;
    for i in 0_usize..100_usize {
        if engine.store_memory(make_memory(&format!("outage-{i}")), None).await.is_err() {
            outage_failures += 1_usize;
        }
    }
    assert_eq!(outage_failures, 100_usize, "all stores during outage should fail");

    // Recovery phase: stores should now succeed
    let mut recovery_ids = Vec::with_capacity(50_usize);
    for i in 0_usize..50_usize {
        let id = engine.store_memory(make_memory(&format!("recovery-{i}")), None).await.unwrap();
        recovery_ids.push(id);
    }

    assert_eq!(recovery_ids.len(), 50_usize, "all post-outage stores should succeed");

    // Verify data integrity: each stored memory is retrievable with correct content
    for (idx, id) in recovery_ids.iter().enumerate() {
        let mem = engine.get_memory(id, None).await.unwrap();
        assert!(mem.is_some(), "memory {id} should exist");
        assert_eq!(mem.unwrap().content, format!("recovery-{idx}"), "memory content should match");
    }
}

// ---------------------------------------------------------------------------
// 3. embedding_cascade_failure
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "extended stress test -- run with --include-ignored"]
async fn chaos_embedding_cascade_failure() {
    let inner = SqliteStore::in_memory().unwrap();
    let store = chaos_store_passthrough(inner);

    // Embedding always fails
    let chaos_embed = ChaosEmbedding::new(Arc::new(DeterministicEmbedding), FaultPlan::Always);

    let engine = RecallEngine::new(store, Arc::new(chaos_embed), LimitsConfig::default(), SearchConfig::default());

    // Store many memories -- all should succeed despite embedding cascade failures
    let total = 200_usize;
    for i in 0_usize..total {
        let _id = engine.store_memory(make_memory(&format!("cascade-{i}")), None).await.unwrap();
    }

    // Wait for all background embed tasks to fail
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // All memories should exist
    let mut filter = MemoryFilter::default();
    filter.limit = Some(total);
    let listed = engine.list_memories(filter, QueryContext::default()).await.unwrap();
    assert_eq!(listed.len(), total, "all {total} memories should be stored");

    // None should have embeddings
    for mem in &listed {
        assert!(!mem.has_embedding, "no memory should have an embedding when embedding always fails");
    }
}
