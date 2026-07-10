//! Concurrent `batch_store` operations — multiple tasks call batch store
//! simultaneously and all resulting IDs must be retrievable.

use std::sync::Arc;

use localhold::{
    config::{LimitsConfig, SearchConfig},
    engine::RecallEngine,
    store::SqliteStore,
    types::MemoryId,
};

use super::workload::{QUICK_TASKS, build_memory};
use crate::helpers::DeterministicEmbedding;

/// Items per batch in quick tests.
const BATCH_SIZE: usize = 5;

fn make_engine() -> RecallEngine<SqliteStore> {
    let store = SqliteStore::in_memory().unwrap();
    RecallEngine::new(store, Arc::new(DeterministicEmbedding), LimitsConfig::default(), SearchConfig::default())
}

#[tokio::test]
async fn concurrent_batch_stores_all_retrievable() {
    let engine = make_engine();
    let stored_ids: Arc<parking_lot::Mutex<Vec<MemoryId>>> = Arc::new(parking_lot::Mutex::new(Vec::new()));

    let mut handles = Vec::new();

    for task_idx in 0_usize..QUICK_TASKS {
        let eng = engine.clone();
        let ids = Arc::clone(&stored_ids);
        handles.push(tokio::spawn(async move {
            let batch: Vec<_> = (0_usize..BATCH_SIZE).map(|i| build_memory(&format!("batch-{task_idx}-item-{i}"), "batch-agent")).collect();
            let sups = vec![None; batch.len()];
            let batch_ids = eng.batch_store(batch, sups).await.unwrap();
            ids.lock().extend(batch_ids);
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let all_ids = stored_ids.lock().clone();
    let expected = QUICK_TASKS * BATCH_SIZE;
    assert_eq!(all_ids.len(), expected, "all batch items should have been stored");

    for id in &all_ids {
        let mem = engine.get_memory(id, None).await.unwrap();
        assert!(mem.is_some(), "batch-stored memory {id} should be retrievable");
    }
}

#[tokio::test]
#[expect(clippy::excessive_nesting, reason = "stress test: spawn + async move + loop is inherently nested")]
async fn overlapping_batch_stores_with_reads() {
    let engine = make_engine();
    let stored_ids: Arc<parking_lot::Mutex<Vec<MemoryId>>> = Arc::new(parking_lot::Mutex::new(Vec::new()));

    let mut handles = Vec::new();

    // Batch writer tasks
    for task_idx in 0_usize..QUICK_TASKS {
        let eng = engine.clone();
        let ids = Arc::clone(&stored_ids);
        handles.push(tokio::spawn(async move {
            for round in 0_usize..3_usize {
                let batch: Vec<_> = (0_usize..BATCH_SIZE)
                    .map(|i| build_memory(&format!("multi-{task_idx}-r{round}-{i}"), "batch-agent"))
                    .collect();
                let batch_ids = {
                    let sups = vec![None; batch.len()];
                    eng.batch_store(batch, sups).await.unwrap()
                };
                ids.lock().extend(batch_ids);
            }
        }));
    }

    // Reader tasks interleaved with batch stores
    for _task_idx in 0_usize..QUICK_TASKS {
        let eng = engine.clone();
        let ids = Arc::clone(&stored_ids);
        handles.push(tokio::spawn(async move {
            for _op in 0_usize..10_usize {
                let snapshot: Vec<MemoryId> = ids.lock().clone();
                for id in snapshot.iter().take(3_usize) {
                    let _mem = eng.get_memory(id, None).await.unwrap();
                }
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let all_ids = stored_ids.lock().clone();
    let expected = QUICK_TASKS * 3_usize * BATCH_SIZE;
    assert_eq!(all_ids.len(), expected, "all batch items across rounds should be stored");
}
