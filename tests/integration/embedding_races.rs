use std::{sync::Arc, time::Duration};

use localhold::{
    server::params::{ReadResponse, RecallResponse, RememberResponse, UpdateResponse},
    types::SearchMode,
};
use serde_json::json;

use super::helpers::{ScriptedEmbedding, ScriptedRule, await_embeddings, call_tool, setup_server_with, sparse_embedding};

#[tokio::test]
async fn stale_embedding_from_remember_does_not_override_post_revise_embedding() {
    let embedding = Arc::new(ScriptedEmbedding::new());
    embedding.set_rule("target-old", ScriptedRule::new(sparse_embedding(&[(0, 1.0)]), Duration::from_millis(250)));
    embedding.set_rule("target-new", ScriptedRule::new(sparse_embedding(&[(1, 1.0)]), Duration::from_millis(20)));
    embedding.set_rule("decoy", ScriptedRule::new(sparse_embedding(&[(0, 0.75), (1, 0.25)]), Duration::ZERO));
    embedding.set_rule("old-query", ScriptedRule::new(sparse_embedding(&[(0, 1.0)]), Duration::ZERO));

    let (client, server) = setup_server_with(embedding).await;

    let decoy: RememberResponse = call_tool(&client, "remember", json!({"content": "decoy"})).await;
    let target: RememberResponse = call_tool(&client, "remember", json!({"content": "target-old"})).await;

    let update: UpdateResponse = call_tool(
        &client,
        "revise",
        json!({
            "id": target.id,
            "content": "target-new"
        }),
    )
    .await;
    assert!(update.updated);

    await_embeddings(&server, Duration::from_secs(5)).await;

    let read: ReadResponse = call_tool(&client, "read", json!({"id": target.id})).await;
    assert_eq!(read.memory.content, "target-new");
    assert!(read.memory.has_embedding);

    let recalled: RecallResponse = call_tool(
        &client,
        "recall",
        json!({
            "query": "old-query",
            "limit": 1_i32
        }),
    )
    .await;

    assert!(recalled.search_mode == SearchMode::Semantic || recalled.search_mode == SearchMode::Hybrid);
    assert_eq!(recalled.count, 1);
    assert_eq!(recalled.results[0].id, decoy.id);

    server.shutdown().await;
}
