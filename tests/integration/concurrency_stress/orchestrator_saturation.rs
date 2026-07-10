//! Saturate the embedding orchestrator with concurrent stores.
//!
//! Uses `SlowDeterministicEmbedding` (with a delay per embed call) to simulate
//! slow embedding generation. Spawns many concurrent store operations and
//! verifies all memories are stored and embeddings eventually complete.

use std::{sync::Arc, time::Duration};

use localhold::{
    config::{LimitsConfig, SearchConfig},
    engine::LocalHoldEngine,
    server::params::RememberResponse,
    store::SqliteStore,
    types::MemoryId,
};
use serde_json::json;

use super::workload::{QUICK_OPS, QUICK_TASKS, STANDARD_OPS, STANDARD_TASKS, build_memory};
use crate::helpers::{SlowDeterministicEmbedding, await_embeddings, call_tool, setup_server_with};

fn make_engine(embedding: Arc<dyn localhold::embedding::EmbeddingProvider>) -> LocalHoldEngine<SqliteStore> {
    let store = SqliteStore::in_memory().unwrap();
    LocalHoldEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default())
}

#[tokio::test]
#[expect(clippy::excessive_nesting, reason = "stress test: spawn + async move + loop is inherently nested")]
async fn orchestrator_handles_burst_of_stores_engine_level() {
    let embedding = Arc::new(SlowDeterministicEmbedding::new(Duration::from_millis(5)));
    let engine = make_engine(embedding);
    let stored_ids: Arc<parking_lot::Mutex<Vec<MemoryId>>> = Arc::new(parking_lot::Mutex::new(Vec::new()));

    let mut handles = Vec::new();
    for task_idx in 0_usize..QUICK_TASKS {
        let eng = engine.clone();
        let ids = Arc::clone(&stored_ids);
        handles.push(tokio::spawn(async move {
            for op in 0_usize..QUICK_OPS {
                let mem = build_memory(&format!("burst-{task_idx}-{op}"), "burst-agent");
                let id = eng.store_memory(mem, None).await.unwrap();
                ids.lock().push(id);
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let all_ids = stored_ids.lock().clone();
    let expected = QUICK_TASKS * QUICK_OPS;
    assert_eq!(all_ids.len(), expected, "all memories should be stored despite slow embeddings");

    // All memories should be retrievable immediately (store completes before embed)
    for id in &all_ids {
        let mem = engine.get_memory(id, None).await.unwrap();
        assert!(mem.is_some(), "memory {id} should be retrievable immediately after store");
    }

    engine.shutdown().await;
}

#[tokio::test]
async fn orchestrator_handles_burst_via_mcp() {
    let embedding = Arc::new(SlowDeterministicEmbedding::new(Duration::from_millis(10)));
    let (client, server) = setup_server_with(embedding).await;

    let mut stored_ids = Vec::new();
    for i in 0_usize..QUICK_OPS {
        let response: RememberResponse = call_tool(
            &client,
            "remember",
            json!({
                "content": format!("mcp-burst-{i}")
            }),
        )
        .await;
        stored_ids.push(response.id);
    }

    assert_eq!(stored_ids.len(), QUICK_OPS, "all MCP stores should succeed");

    // Wait for all embeddings to complete
    await_embeddings(&server, Duration::from_secs(30)).await;

    server.shutdown().await;
}

#[tokio::test]
#[ignore = "extended orchestrator saturation test"]
#[expect(clippy::excessive_nesting, reason = "stress test: spawn + async move + loop is inherently nested")]
async fn orchestrator_saturation_standard() {
    let embedding = Arc::new(SlowDeterministicEmbedding::new(Duration::from_millis(2)));
    let engine = make_engine(embedding);
    let stored_ids: Arc<parking_lot::Mutex<Vec<MemoryId>>> = Arc::new(parking_lot::Mutex::new(Vec::new()));

    let mut handles = Vec::new();
    for task_idx in 0_usize..STANDARD_TASKS {
        let eng = engine.clone();
        let ids = Arc::clone(&stored_ids);
        handles.push(tokio::spawn(async move {
            for op in 0_usize..STANDARD_OPS {
                let mem = build_memory(&format!("sat-{task_idx}-{op}"), "sat-agent");
                let id = eng.store_memory(mem, None).await.unwrap();
                ids.lock().push(id);
            }
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    let all_ids = stored_ids.lock().clone();
    let expected = STANDARD_TASKS * STANDARD_OPS;
    assert_eq!(all_ids.len(), expected, "all memories should be stored under saturation");

    engine.shutdown().await;
}
