//! Chunk scheduling and per-memory persistence for explicit embedding batches.

use std::sync::Arc;

use tracing::{info, warn};

use super::{ActiveEmbedClaimGuard, EmbedWork, EmbeddingOrchestrator, InFlightEmbed, apply_embedding_result, release_work_claim};
use crate::{
    background_tasks::{BackgroundTaskKind, EmbedAdmission},
    embedding::batch::{BatchEmbeddingExecutor, BatchEmbeddingResult},
    store::{MemoryStore, ReembedClaim},
};

enum RejectedSpawn {
    RunInline,
    ReleaseClaims,
}

struct TrackedEmbedWork {
    work: EmbedWork,
    _inflight: InFlightEmbed,
    _active_claim: Option<ActiveEmbedClaimGuard>,
}

#[expect(clippy::multiple_inherent_impl, reason = "bulk scheduling is split from the core orchestrator for readability")]
impl<S: MemoryStore + Clone + std::fmt::Debug + 'static> EmbeddingOrchestrator<S> {
    pub(super) async fn spawn_embed_batches_or_run_inline(&self, admission: &EmbedAdmission, work: Vec<EmbedWork>) -> usize {
        self.schedule_batches(admission, work, RejectedSpawn::RunInline).await
    }

    pub(crate) async fn spawn_claimed_embed_batches_or_run_inline(&self, admission: &EmbedAdmission, claims: Vec<ReembedClaim>) -> usize {
        let work = claims
            .into_iter()
            .map(|claim| EmbedWork {
                id: claim.id,
                content: Arc::from(claim.content),
                expected_revision: claim.embedding_revision,
                claim_token: Some(claim.claim_token),
            })
            .collect();
        self.schedule_batches(admission, work, RejectedSpawn::ReleaseClaims).await
    }

    async fn schedule_batches(&self, admission: &EmbedAdmission, work: Vec<EmbedWork>, rejected_spawn: RejectedSpawn) -> usize {
        let mut work = work.into_iter();
        let mut scheduled = 0_usize;
        loop {
            let chunk: Vec<EmbedWork> = work.by_ref().take(self.batch_executor.chunk_size()).collect();
            if chunk.is_empty() {
                return scheduled;
            }
            let count = self.schedule_chunk(admission, chunk, &rejected_spawn).await;
            scheduled = scheduled.saturating_add(count);
        }
    }

    async fn schedule_chunk(&self, admission: &EmbedAdmission, work: Vec<EmbedWork>, rejected_spawn: &RejectedSpawn) -> usize {
        let tracked = self.prepare_work(work).await;
        if tracked.is_empty() {
            return 0;
        }

        let accepted: Vec<EmbedWork> = tracked.iter().map(|item| item.work.clone()).collect();
        let scheduled = tracked.len();
        let executor = self.batch_executor.clone();
        let store = self.store.clone();
        if admission.spawn(BackgroundTaskKind::Embed, async move {
            run_batch(executor, store, tracked).await;
        }) {
            return scheduled;
        }

        match rejected_spawn {
            RejectedSpawn::RunInline => {
                warn!(item_count = scheduled, "embedding batch admission timed out during shutdown; embedding inline");
                let tracked = self.prepare_work(accepted).await;
                let completed = tracked.len();
                run_batch(self.batch_executor.clone(), self.store.clone(), tracked).await;
                completed
            }
            RejectedSpawn::ReleaseClaims => {
                warn!(item_count = scheduled, "claimed embedding batch admission timed out during shutdown; releasing claims");
                for work in accepted {
                    release_work_claim(&self.store, &work).await;
                }
                0
            }
        }
    }

    async fn prepare_work(&self, work: Vec<EmbedWork>) -> Vec<TrackedEmbedWork> {
        let mut tracked = Vec::with_capacity(work.len());
        for work in work {
            let Some(inflight) = self.begin_inflight_embed(work.id, work.expected_revision) else {
                info!(memory_id = %work.id, expected_revision = work.expected_revision, "embed task already in flight, skipping duplicate");
                release_work_claim(&self.store, &work).await;
                continue;
            };
            let active_claim = work.claim_token.as_deref().map(|token| self.track_active_claim(work.id, work.expected_revision, token));
            tracked.push(TrackedEmbedWork {
                work,
                _inflight: inflight,
                _active_claim: active_claim,
            });
        }
        tracked
    }
}

async fn run_batch<S: MemoryStore>(executor: BatchEmbeddingExecutor, store: S, tracked: Vec<TrackedEmbedWork>) {
    if tracked.is_empty() {
        return;
    }
    let contents: Vec<Arc<str>> = tracked.iter().map(|item| Arc::clone(&item.work.content)).collect();
    match executor.execute_chunk(&contents).await {
        BatchEmbeddingResult::PerItem(results) => {
            debug_assert_eq!(tracked.len(), results.len(), "batch executor must return one result per tracked item");
            for (tracked, result) in tracked.into_iter().zip(results) {
                apply_embedding_result(&store, &tracked.work, result).await;
            }
        }
        BatchEmbeddingResult::BatchFailed(error) => {
            warn!(item_count = tracked.len(), error = %error, "embedding batch failed after retries");
            for tracked in tracked {
                release_work_claim(&store, &tracked.work).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use tokio::sync::Notify;

    use super::EmbeddingOrchestrator;
    use crate::{
        background_tasks::BackgroundTasks,
        embedding::{BoxFuture, EmbeddingProvider},
        error::EmbeddingError,
        store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
        types::{AccessPolicy, Memory, MemoryUpdate, Provenance},
    };

    struct FailingBatchProvider {
        batch_calls: AtomicUsize,
        single_calls: AtomicUsize,
    }

    impl EmbeddingProvider for FailingBatchProvider {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            let _previous = self.single_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Ok(test_embedding()) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }

        fn embed_batch<'a>(&'a self, _texts: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
            let _previous = self.batch_calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Err(EmbeddingError::Transient("provider unavailable".into())) })
        }
    }

    struct BlockingBatchProvider {
        started: Notify,
        release: Notify,
    }

    impl EmbeddingProvider for BlockingBatchProvider {
        fn embed<'a>(&'a self, _text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
            Box::pin(async { Ok(test_embedding()) })
        }

        fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
            Box::pin(async { Ok(()) })
        }

        fn embed_batch<'a>(&'a self, texts: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
            Box::pin(async move {
                self.started.notify_waiters();
                self.release.notified().await;
                Ok(texts.iter().map(|_text| test_embedding()).collect())
            })
        }
    }

    fn test_embedding() -> Vec<f32> {
        let mut embedding = vec![0.0_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS];
        embedding[0] = 1.0;
        embedding
    }

    fn test_memory(content: &str) -> Memory {
        Memory::new_for_test(content.to_owned(), Vec::new(), Provenance::default(), AccessPolicy::Public)
    }

    async fn store_unembedded(store: &SqliteStore, contents: &[&str]) -> Vec<crate::types::MemoryId> {
        let mut ids = Vec::with_capacity(contents.len());
        for content in contents {
            ids.push(store.store(&test_memory(content), None).await.unwrap());
        }
        ids
    }

    #[tokio::test]
    async fn failed_claimed_batch_releases_every_claim_without_single_fallback() {
        let store = SqliteStore::in_memory().unwrap();
        let ids = store_unembedded(&store, &["first", "second"]).await;
        let claims = store.claim_for_reembed(2).await.unwrap();
        let original_tokens: HashMap<_, _> = claims.iter().map(|claim| (claim.id, claim.claim_token.clone())).collect();
        let provider = Arc::new(FailingBatchProvider {
            batch_calls: AtomicUsize::new(0),
            single_calls: AtomicUsize::new(0),
        });
        let provider_for_orchestrator: Arc<dyn EmbeddingProvider> = Arc::<FailingBatchProvider>::clone(&provider);
        let background_tasks = BackgroundTasks::new();
        let orchestrator = EmbeddingOrchestrator::new(store.clone(), provider_for_orchestrator, Arc::clone(&background_tasks), 32);
        let admission = background_tasks.begin_embed_admission().unwrap();

        assert_eq!(orchestrator.spawn_claimed_embed_batches_or_run_inline(&admission, claims).await, 2);
        drop(admission);
        orchestrator.shutdown(Duration::from_secs(1)).await;

        assert_eq!(provider.batch_calls.load(Ordering::Relaxed), 1);
        assert_eq!(provider.single_calls.load(Ordering::Relaxed), 0);
        let available = store.claim_for_reembed(2).await.unwrap();
        assert_eq!(available.len(), ids.len());
        for claim in available {
            assert_ne!(Some(&claim.claim_token), original_tokens.get(&claim.id));
        }
    }

    #[tokio::test]
    async fn successful_batch_skips_only_stale_revision() {
        let store = SqliteStore::in_memory().unwrap();
        let ids = store_unembedded(&store, &["current", "will change"]).await;
        let claims = store.claim_for_reembed(2).await.unwrap();
        let provider = Arc::new(BlockingBatchProvider {
            started: Notify::new(),
            release: Notify::new(),
        });
        let provider_for_orchestrator: Arc<dyn EmbeddingProvider> = Arc::<BlockingBatchProvider>::clone(&provider);
        let background_tasks = BackgroundTasks::new();
        let orchestrator = EmbeddingOrchestrator::new(store.clone(), provider_for_orchestrator, Arc::clone(&background_tasks), 32);
        let admission = background_tasks.begin_embed_admission().unwrap();
        let started = provider.started.notified();
        tokio::pin!(started);
        let _registered = started.as_mut().enable();

        assert_eq!(orchestrator.spawn_claimed_embed_batches_or_run_inline(&admission, claims).await, 2);
        started.await;
        let update = MemoryUpdate {
            content: Some("changed".into()),
            ..MemoryUpdate::default()
        };
        assert!(store.update(&ids[1], &update).await.unwrap());
        provider.release.notify_waiters();
        drop(admission);
        orchestrator.shutdown(Duration::from_secs(1)).await;

        assert!(store.get(&ids[0], None).await.unwrap().unwrap().has_embedding);
        assert!(!store.get(&ids[1], None).await.unwrap().unwrap().has_embedding);
    }
}
