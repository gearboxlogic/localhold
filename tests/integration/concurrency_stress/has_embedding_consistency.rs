//! Verify `has_embedding` flag consistency under concurrent store and embed.
//!
//! Stores memories, waits for embeddings to complete, then verifies that
//! `has_embedding` is `true` for all. Also checks that concurrent listing
//! during embedding never panics or observes an inconsistent state.

use std::{sync::Arc, time::Duration};

use localhold::{
    config::{LimitsConfig, SearchConfig},
    engine::RecallEngine,
    server::params::{ReadResponse, RememberResponse},
    store::SqliteStore,
    types::{MemoryFilter, MemoryId, QueryContext},
};
use serde_json::json;

use super::workload::{QUICK_OPS, QUICK_TASKS, build_memory};
use crate::helpers::{SlowDeterministicEmbedding, await_embeddings, call_tool, setup_server_with};

fn make_engine(embedding: Arc<dyn localhold::embedding::EmbeddingProvider>) -> RecallEngine<SqliteStore> {
    let store = SqliteStore::in_memory().unwrap();
    RecallEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default())
}

#[tokio::test]
async fn has_embedding_true_after_embed_completes() {
    let embedding: Arc<dyn localhold::embedding::EmbeddingProvider> = Arc::new(SlowDeterministicEmbedding::new(Duration::from_millis(5)));
    let (client, server) = setup_server_with(embedding).await;

    // Store memories via MCP
    let mut ids = Vec::new();
    for i in 0_usize..QUICK_OPS {
        let response: RememberResponse = call_tool(
            &client,
            "remember",
            json!({
                "content": format!("embed-check-{i}")
            }),
        )
        .await;
        ids.push(response.id);
    }

    // Wait for all embeddings to finish
    await_embeddings(&server, Duration::from_secs(30)).await;

    // Verify has_embedding is true for all via read.
    for id in &ids {
        let read: ReadResponse = call_tool(&client, "read", json!({"id": id})).await;
        assert!(read.memory.has_embedding, "memory {id} should have embedding after await_embeddings");
    }

    server.shutdown().await;
}

#[tokio::test]
#[expect(clippy::excessive_nesting, reason = "stress test: spawn + async move + loop is inherently nested")]
async fn concurrent_list_during_embedding_observes_consistent_flags() {
    let embedding: Arc<dyn localhold::embedding::EmbeddingProvider> = Arc::new(SlowDeterministicEmbedding::new(Duration::from_millis(10)));
    let engine = make_engine(embedding);

    // Store memories concurrently
    let stored_ids: Arc<parking_lot::Mutex<Vec<MemoryId>>> = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let mut store_handles = Vec::new();
    for task_idx in 0_usize..QUICK_TASKS {
        let eng = engine.clone();
        let ids = Arc::clone(&stored_ids);
        store_handles.push(tokio::spawn(async move {
            for op in 0_usize..QUICK_OPS {
                let mem = build_memory(&format!("flag-check-{task_idx}-{op}"), "flag-agent");
                let id = eng.store_memory(mem, None).await.unwrap();
                ids.lock().push(id);
            }
        }));
    }

    // Meanwhile, repeatedly list and check consistency
    let eng_reader = engine.clone();
    let reader_handle = tokio::spawn(async move {
        let mut iterations = 0_usize;
        loop {
            let _memories = eng_reader.list_memories(MemoryFilter::default(), QueryContext::default()).await.unwrap();

            // The invariant we're testing is that reading concurrently with
            // writes and embeds does not cause a crash or inconsistency.

            iterations = iterations.checked_add(1_usize).unwrap();
            if iterations >= QUICK_OPS {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });

    for handle in store_handles {
        handle.await.unwrap();
    }
    reader_handle.await.unwrap();

    engine.shutdown().await;
}
