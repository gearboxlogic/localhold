//! Concurrent scope reassignment — stores memories with different scopes,
//! then concurrently reassigns them. Verifies no memory is lost and final
//! scope assignments are valid.

use std::sync::Arc;

use localhold::{
    config::{LimitsConfig, SearchConfig},
    engine::RecallEngine,
    store::SqliteStore,
    types::{MemoryFilter, QueryContext},
};

use super::workload::{QUICK_OPS, QUICK_TASKS, build_scoped_memory};
use crate::helpers::DeterministicEmbedding;

fn make_engine() -> RecallEngine<SqliteStore> {
    let store = SqliteStore::in_memory().unwrap();
    RecallEngine::new(store, Arc::new(DeterministicEmbedding), LimitsConfig::default(), SearchConfig::default())
}

#[tokio::test]
async fn concurrent_scope_reassignments_no_data_loss() {
    let engine = make_engine();

    // Store memories across several scopes
    let scope_count = QUICK_TASKS;
    let mut total_stored = 0_usize;
    for scope_idx in 0_usize..scope_count {
        for op in 0_usize..QUICK_OPS {
            let scope = format!("scope-{scope_idx}");
            let mem = build_scoped_memory(&format!("content-{scope_idx}-{op}"), "agent", &scope);
            let _id = engine.store_memory(mem, None).await.unwrap();
            total_stored = total_stored.checked_add(1_usize).unwrap();
        }
    }

    // Concurrently reassign scopes: each task moves one scope to a new destination
    let mut handles = Vec::new();
    for scope_idx in 0_usize..scope_count {
        let eng = engine.clone();
        let from = format!("scope-{scope_idx}");
        let to = format!("reassigned-{scope_idx}");
        handles.push(tokio::spawn(async move {
            let _count = eng.reassign_scope(&from, &to, None, "agent").await.unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }

    // Verify no data loss: total memories should be the same
    let all = engine.list_memories(MemoryFilter::default(), QueryContext::default()).await.unwrap();
    assert_eq!(all.len(), total_stored, "no memories should be lost after reassignment");

    // Verify original scopes are now empty
    for scope_idx in 0_usize..scope_count {
        let mut filter = MemoryFilter::default();
        filter.scope = Some(format!("scope-{scope_idx}"));
        let remaining = engine.list_memories(filter, QueryContext::default()).await.unwrap();
        assert!(remaining.is_empty(), "original scope-{scope_idx} should be empty after reassignment");
    }
}

#[tokio::test]
async fn chained_scope_reassignments() {
    let engine = make_engine();

    // Store memories in chain-start scope
    for op in 0_usize..QUICK_OPS {
        let mem = build_scoped_memory(&format!("chain-{op}"), "agent", "chain-start");
        let _id = engine.store_memory(mem, None).await.unwrap();
    }

    // Sequentially reassign through a chain: start -> mid-0 -> mid-1 -> ... -> final
    let chain_length = QUICK_TASKS;
    let mut current_scope = "chain-start".to_owned();
    for step in 0_usize..chain_length {
        let next = if step == chain_length.checked_sub(1_usize).unwrap() {
            "chain-end".to_owned()
        } else {
            format!("chain-mid-{step}")
        };
        let count = engine.reassign_scope(&current_scope, &next, None, "agent").await.unwrap();
        #[expect(clippy::as_conversions, reason = "usize constant QUICK_OPS is always valid as u64")]
        {
            assert_eq!(count, QUICK_OPS as u64, "all memories should be reassigned at step {step}");
        }
        current_scope = next;
    }

    // Verify all ended up in the final scope
    let mut filter = MemoryFilter::default();
    filter.scope = Some("chain-end".to_owned());
    let final_memories = engine.list_memories(filter, QueryContext::default()).await.unwrap();
    assert_eq!(final_memories.len(), QUICK_OPS, "all memories should end up in chain-end");
}
