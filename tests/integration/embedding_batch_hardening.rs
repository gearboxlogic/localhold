use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use localhold::{
    config::{LimitsConfig, SearchConfig},
    embedding::{BoxFuture, EmbeddingProvider},
    engine::{LocalHoldEngine, ReembedOutcome, ReembedRequest},
    error::EmbeddingError,
    store::{MemoryReader as _, MemoryWriter as _, SqliteStore},
    types::{AccessPolicy, Memory, Provenance},
};
use parking_lot::Mutex;
use tokio::sync::{Notify, Semaphore};

struct RecordingBatchProvider {
    content_calls: Mutex<HashMap<String, usize>>,
}

impl RecordingBatchProvider {
    fn new() -> Self {
        Self {
            content_calls: Mutex::new(HashMap::new()),
        }
    }

    fn call_count(&self, content: &str) -> usize {
        self.content_calls.lock().get(content).copied().unwrap_or_default()
    }

    fn record(&self, text: &str) {
        let mut calls = self.content_calls.lock();
        let count = calls.entry(text.to_owned()).or_default();
        *count = count.saturating_add(1);
        drop(calls);
    }
}

impl EmbeddingProvider for RecordingBatchProvider {
    fn embed<'a>(&'a self, text: &'a str) -> BoxFuture<'a, Result<Vec<f32>, EmbeddingError>> {
        self.record(text);
        Box::pin(async { Ok(test_embedding()) })
    }

    fn health_check(&self) -> BoxFuture<'_, Result<(), EmbeddingError>> {
        Box::pin(async { Ok(()) })
    }

    fn embed_batch<'a>(&'a self, texts: &'a [&'a str]) -> BoxFuture<'a, Result<Vec<Vec<f32>>, EmbeddingError>> {
        for text in texts {
            self.record(text);
        }
        Box::pin(async move { Ok(texts.iter().map(|_text| test_embedding()).collect()) })
    }
}

struct BlockingBatchProvider {
    active: AtomicUsize,
    peak: AtomicUsize,
    started: AtomicUsize,
    started_notify: Notify,
    release: Semaphore,
}

impl BlockingBatchProvider {
    fn new() -> Self {
        Self {
            active: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            started: AtomicUsize::new(0),
            started_notify: Notify::new(),
            release: Semaphore::new(0),
        }
    }

    async fn wait_for_started(&self, target: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            while self.started.load(Ordering::Acquire) < target {
                self.started_notify.notified().await;
            }
        })
        .await
        .unwrap();
    }

    fn begin_request(&self) {
        let active = self.active.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        let _previous = self.peak.fetch_max(active, Ordering::AcqRel);
        let _previous = self.started.fetch_add(1, Ordering::AcqRel);
        self.started_notify.notify_waiters();
    }
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
            self.begin_request();
            let _permit = self.release.acquire().await.map_err(|_closed| EmbeddingError::Disabled)?;
            let _previous = self.active.fetch_sub(1, Ordering::AcqRel);
            Ok(texts.iter().map(|_text| test_embedding()).collect())
        })
    }
}

fn test_embedding() -> Vec<f32> {
    let mut embedding = vec![0.0_f32; SqliteStore::DEFAULT_TEST_DIMENSIONS];
    embedding[0] = 1.0;
    embedding
}

fn test_memory(content: String) -> Memory {
    Memory::new_for_test(content, Vec::new(), Provenance::default(), AccessPolicy::Public)
}

const fn queued_count(outcome: &ReembedOutcome) -> usize {
    if let ReembedOutcome::Queued(count) = outcome { *count } else { 0 }
}

#[tokio::test]
async fn two_instances_partition_durable_reembed_claims() {
    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("shared.db");
    let first_store = SqliteStore::open(&database_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    let second_store = SqliteStore::open(&database_path, SqliteStore::DEFAULT_TEST_DIMENSIONS).unwrap();
    let contents: Vec<String> = (0_usize..40).map(|index| format!("shared backlog {index}")).collect();
    let mut ids = Vec::with_capacity(contents.len());
    for content in &contents {
        ids.push(first_store.store(&test_memory(content.clone()), None).await.unwrap());
    }

    let provider = Arc::new(RecordingBatchProvider::new());
    let mut limits = LimitsConfig::default();
    limits.max_reembed_limit = contents.len();
    limits.embedding_batch_size = 8;
    let first_provider: Arc<dyn EmbeddingProvider> = Arc::<RecordingBatchProvider>::clone(&provider);
    let second_provider: Arc<dyn EmbeddingProvider> = Arc::<RecordingBatchProvider>::clone(&provider);
    let first_engine = LocalHoldEngine::new(first_store.clone(), first_provider, limits.clone(), SearchConfig::default());
    let second_engine = LocalHoldEngine::new(second_store, second_provider, limits, SearchConfig::default());

    let (first, second) = tokio::join!(
        first_engine.reembed(ReembedRequest::Bulk { limit: contents.len() }),
        second_engine.reembed(ReembedRequest::Bulk { limit: contents.len() })
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(queued_count(&first).saturating_add(queued_count(&second)), contents.len());
    tokio::join!(
        first_engine.shutdown_for_test(Duration::from_secs(2)),
        second_engine.shutdown_for_test(Duration::from_secs(2))
    );

    for (id, content) in ids.into_iter().zip(contents) {
        assert_eq!(provider.call_count(&content), 1, "content should be embedded by exactly one instance: {content}");
        assert!(first_store.get(&id, None).await.unwrap().unwrap().has_embedding);
    }
}

#[tokio::test]
async fn explicit_batch_chunks_obey_global_request_concurrency() {
    let store = SqliteStore::in_memory().unwrap();
    let provider = Arc::new(BlockingBatchProvider::new());
    let mut limits = LimitsConfig::default();
    limits.max_batch_size = 100;
    limits.embedding_batch_size = 10;
    limits.max_concurrent_embedding_requests = 2;
    let engine_provider: Arc<dyn EmbeddingProvider> = Arc::<BlockingBatchProvider>::clone(&provider);
    let engine = LocalHoldEngine::new(store, engine_provider, limits, SearchConfig::default());
    let memories: Vec<Memory> = (0_usize..50).map(|index| test_memory(format!("load item {index}"))).collect();

    let ids = engine.batch_store(memories, vec![None; 50]).await.unwrap();
    provider.wait_for_started(2).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(provider.started.load(Ordering::Acquire), 2, "later chunks must wait for a provider permit");
    assert_eq!(provider.peak.load(Ordering::Acquire), 2);

    provider.release.add_permits(5);
    engine.shutdown_for_test(Duration::from_secs(2)).await;
    assert_eq!(provider.started.load(Ordering::Acquire), 5);
    assert_eq!(provider.peak.load(Ordering::Acquire), 2);
    assert_eq!(ids.len(), 50);
}
