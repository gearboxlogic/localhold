use std::{sync::Arc, time::Duration};

use localhold::server::params::RememberResponse;
use serde_json::json;

use super::helpers::{ScriptedEmbedding, ScriptedRule, call_tool, setup_server_with, sparse_embedding};

/// Total number of memories to store during the soak test.
const SOAK_TOTAL_MEMORIES: i32 = 400;

/// Interval (in iterations) between reap/check cycles.
const SOAK_CHECK_INTERVAL: i32 = 25;

/// Maximum number of tracked tasks allowed at any checkpoint.
const SOAK_MAX_TRACKED_TASKS: usize = 200;

#[tokio::test]
#[ignore = "slow soak-style regression test"]
#[expect(clippy::let_underscore_must_use, reason = "reap count is not needed; we assert on tracked_task_count instead")]
async fn tracked_embedding_tasks_are_reaped_under_sustained_load() {
    let embedding = Arc::new(ScriptedEmbedding::new());
    embedding.set_rule("soak-content", ScriptedRule::new(sparse_embedding(&[(2, 1.0)]), Duration::from_millis(5)));

    let (client, server) = setup_server_with(embedding).await;

    for idx in 0_i32..SOAK_TOTAL_MEMORIES {
        let _stored: RememberResponse = call_tool(
            &client,
            "remember",
            json!({
                "content": "soak-content",
                "agent_label": "load-bot",
                "scope": format!("soak/c-{idx}")
            }),
        )
        .await;

        #[expect(clippy::integer_division_remainder_used, reason = "intentional periodic check every 25 iterations")]
        if idx % SOAK_CHECK_INTERVAL == 0_i32 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = server.reap_completed_tasks_for_test();
            assert!(server.tracked_task_count() < SOAK_MAX_TRACKED_TASKS, "tracked tasks should not grow unbounded");
        }
    }

    tokio::time::sleep(Duration::from_secs(1)).await;
    let _ = server.reap_completed_tasks_for_test();
    assert_eq!(server.tracked_task_count(), 0, "all completed tasks should be reaped");

    server.shutdown().await;
}
