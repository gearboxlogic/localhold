//! Race between content updates and embedding generation.
//!
//! Stores memories, then concurrently updates content while the embedding
//! orchestrator generates embeddings in the background. Verifies no corruption
//! and that the final content matches the last applied update.

use std::{sync::Arc, time::Duration};

use localhold::{
    config::{LimitsConfig, SearchConfig},
    engine::LocalHoldEngine,
    store::SqliteStore,
    types::{MemoryId, MemoryUpdate},
};

use super::workload::{QUICK_OPS, QUICK_TASKS, build_memory};
use crate::helpers::{DeterministicEmbedding, SlowDeterministicEmbedding};

fn make_engine(embedding: Arc<dyn localhold::embedding::EmbeddingProvider>) -> LocalHoldEngine<SqliteStore> {
    let store = SqliteStore::in_memory().unwrap();
    LocalHoldEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default())
}

#[tokio::test]
#[expect(clippy::excessive_nesting, reason = "stress test: spawn + async move + loop is inherently nested")]
async fn update_during_embedding_preserves_latest_content() {
    // Use a slow embedding to increase the window for races
    let engine = make_engine(Arc::new(SlowDeterministicEmbedding::new(Duration::from_millis(10))));

    // Store initial memories
    let mut ids = Vec::new();
    for i in 0_usize..QUICK_TASKS {
        let mem = build_memory(&format!("initial-{i}"), "owner");
        let id = engine.store_memory(mem, None).await.unwrap();
        ids.push(id);
    }

    // Concurrently update each memory multiple times while embeddings are being generated
    let mut handles = Vec::new();
    for (idx, id) in ids.iter().copied().enumerate() {
        let eng = engine.clone();
        handles.push(tokio::spawn(async move {
            for op in 0_usize..QUICK_OPS {
                let mut update = MemoryUpdate::default();
                update.content = Some(format!("updated-{idx}-v{op}"));
                let _outcome = eng.update_memory(id, update, "owner").await.unwrap();
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Verify final content matches last update
    let last_version = QUICK_OPS - 1_usize;
    for (idx, id) in ids.iter().enumerate() {
        let mem = engine.get_memory(id, None).await.unwrap().unwrap();
        let expected = format!("updated-{idx}-v{last_version}");
        assert_eq!(mem.content, expected, "memory {id} should have the latest update");
    }

    engine.shutdown().await;
}

#[tokio::test]
#[expect(clippy::excessive_nesting, reason = "stress test: spawn + async move + loop is inherently nested")]
async fn concurrent_store_and_update_no_corruption() {
    let engine = make_engine(Arc::new(DeterministicEmbedding));
    let ids: Arc<parking_lot::Mutex<Vec<MemoryId>>> = Arc::new(parking_lot::Mutex::new(Vec::new()));

    let mut handles = Vec::new();

    // Spawn store tasks
    for task_idx in 0_usize..QUICK_TASKS {
        let eng = engine.clone();
        let ids_ref = Arc::clone(&ids);
        handles.push(tokio::spawn(async move {
            for op in 0_usize..QUICK_OPS {
                let mem = build_memory(&format!("store-{task_idx}-{op}"), "owner");
                let id = eng.store_memory(mem, None).await.unwrap();
                ids_ref.lock().push(id);
            }
        }));
    }

    // Spawn update tasks that update the first available memory
    for _task_idx in 0_usize..QUICK_TASKS {
        let eng = engine.clone();
        let ids_ref = Arc::clone(&ids);
        handles.push(tokio::spawn(async move {
            for op in 0_usize..QUICK_OPS {
                let snapshot: Vec<MemoryId> = ids_ref.lock().clone();
                if let Some(target_id) = snapshot.first().copied() {
                    let mut update = MemoryUpdate::default();
                    update.content = Some(format!("concurrent-update-{op}"));
                    let _outcome = eng.update_memory(target_id, update, "owner").await.unwrap();
                }
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Verify all stored memories are retrievable and non-empty
    let all_ids = ids.lock().clone();
    for id in &all_ids {
        let mem = engine.get_memory(id, None).await.unwrap();
        assert!(mem.is_some(), "memory {id} should still exist");
        assert!(!mem.unwrap().content.is_empty(), "memory content should not be empty");
    }
}
