#![expect(missing_docs, reason = "benchmark binary — no public API to document")]
#![expect(clippy::expect_used, reason = "benchmarks use expect for infallible setup")]
#![expect(unused_crate_dependencies, reason = "dev-dependencies shared across bench/test targets")]

mod common;

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use localhold::{
    config::{LimitsConfig, SearchConfig},
    embedding::NoopEmbedding,
    engine::RecallEngine,
    store::{MemoryReader as _, SqliteStore},
    types::{MemoryFilter, QueryContext},
};

use crate::common::seeder::BenchSeeder;

/// Seed a file-backed store with `count` memories and return the engine.
///
/// Uses `tempfile` for realistic I/O characteristics.
#[expect(unused_results, reason = "batch_store IDs are not needed during setup seeding")]
fn seeded_engine(count: usize, tmp_path: &std::path::Path) -> RecallEngine<SqliteStore> {
    let store = SqliteStore::open(tmp_path, 768_usize).expect("open file-backed store");
    let embedding: Arc<dyn localhold::embedding::EmbeddingProvider> = Arc::new(NoopEmbedding::new());
    let engine = RecallEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default());

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut seeder = BenchSeeder::new(42_u64);
    let memories = seeder.memories(count);

    // Store in batches of 100 to avoid oversized transactions
    for chunk in memories.chunks(100_usize) {
        let batch: Vec<_> = chunk.to_vec();
        rt.block_on(engine.batch_store(batch, vec![])).expect("batch store");
    }

    engine
}

async fn run_search(store: &SqliteStore) {
    let filter = MemoryFilter::default();
    let ctx = QueryContext::default();
    let _results = store.search_by_text("memory recall search", 10_usize, &filter, &ctx).await;
}

#[expect(unused_results, reason = "criterion bench_with_input returns a builder ref we do not chain")]
fn search_latency_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_latency");
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    for count in [100_usize, 1_000_usize, 5_000_usize] {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        let engine = seeded_engine(count, tmp.path());
        let store = engine.store().clone();

        group.bench_with_input(BenchmarkId::from_parameter(count), &store, |b, store| {
            b.to_async(&rt).iter(|| run_search(store));
        });
    }

    group.finish();
}

criterion_group!(benches, search_latency_benchmark);
criterion_main!(benches);
