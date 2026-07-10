//! Concurrent readers and writers — multiple tasks store memories while others
//! list and retrieve them. Verifies no panics and all successful stores are
//! retrievable.

use std::sync::Arc;

use localhold::{
    config::{LimitsConfig, SearchConfig},
    engine::LocalHoldEngine,
    store::SqliteStore,
    types::{MemoryFilter, MemoryId, QueryContext},
};

use super::workload::{QUICK_OPS, QUICK_TASKS, STANDARD_OPS, STANDARD_TASKS, build_memory};
use crate::helpers::DeterministicEmbedding;

fn make_engine() -> LocalHoldEngine<SqliteStore> {
    let store = SqliteStore::in_memory().unwrap();
    LocalHoldEngine::new(store, Arc::new(DeterministicEmbedding), LimitsConfig::default(), SearchConfig::default())
}

#[tokio::test]
#[expect(clippy::excessive_nesting, reason = "stress test: spawn + async move + loop is inherently nested")]
async fn concurrent_readers_and_writers_quick() {
    let engine = make_engine();
    let stored_ids: Arc<parking_lot::Mutex<Vec<MemoryId>>> = Arc::new(parking_lot::Mutex::new(Vec::new()));

    let mut handles = Vec::new();

    // Writer tasks
    for task_idx in 0_usize..QUICK_TASKS {
        let eng = engine.clone();
        let ids = Arc::clone(&stored_ids);
        handles.push(tokio::spawn(async move {
            for op in 0_usize..QUICK_OPS {
                let mem = build_memory(&format!("writer-{task_idx}-op-{op}"), "stress-agent");
                let id = eng.store_memory(mem, None).await.unwrap();
                ids.lock().push(id);
            }
        }));
    }

    // Reader tasks (list)
    for _task_idx in 0_usize..QUICK_TASKS {
        let eng = engine.clone();
        handles.push(tokio::spawn(async move {
            for _op in 0_usize..QUICK_OPS {
                let _memories = eng.list_memories(MemoryFilter::default(), QueryContext::default()).await.unwrap();
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all stored IDs are retrievable
    let ids = stored_ids.lock().clone();
    let expected_count = QUICK_TASKS * QUICK_OPS;
    assert_eq!(ids.len(), expected_count, "all writer operations should have produced IDs");

    for id in &ids {
        let mem = engine.get_memory(id, None).await.unwrap();
        assert!(mem.is_some(), "stored memory {id} should be retrievable");
    }
}

#[tokio::test]
#[ignore = "extended concurrency stress test"]
#[expect(clippy::excessive_nesting, reason = "stress test: spawn + async move + loop is inherently nested")]
async fn concurrent_readers_and_writers_standard() {
    let engine = make_engine();
    let stored_ids: Arc<parking_lot::Mutex<Vec<MemoryId>>> = Arc::new(parking_lot::Mutex::new(Vec::new()));

    let mut handles = Vec::new();

    // Writer tasks
    for task_idx in 0_usize..STANDARD_TASKS {
        let eng = engine.clone();
        let ids = Arc::clone(&stored_ids);
        handles.push(tokio::spawn(async move {
            for op in 0_usize..STANDARD_OPS {
                let mem = build_memory(&format!("writer-{task_idx}-op-{op}"), "stress-agent");
                let id = eng.store_memory(mem, None).await.unwrap();
                ids.lock().push(id);
            }
        }));
    }

    // Reader tasks (list + get)
    for _task_idx in 0_usize..STANDARD_TASKS {
        let eng = engine.clone();
        let ids = Arc::clone(&stored_ids);
        handles.push(tokio::spawn(async move {
            for _op in 0_usize..STANDARD_OPS {
                let _memories = eng.list_memories(MemoryFilter::default(), QueryContext::default()).await.unwrap();
                // Try to get a stored memory if any exist
                let snapshot: Vec<MemoryId> = ids.lock().clone();
                if let Some(id) = snapshot.first() {
                    let _mem = eng.get_memory(id, None).await.unwrap();
                }
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let ids = stored_ids.lock().clone();
    let expected_count = STANDARD_TASKS * STANDARD_OPS;
    assert_eq!(ids.len(), expected_count, "all writer operations should have produced IDs");
}
