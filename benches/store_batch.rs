#![expect(missing_docs, reason = "benchmark binary — no public API to document")]
#![expect(clippy::expect_used, reason = "benchmarks use expect for infallible setup")]
#![expect(unused_crate_dependencies, reason = "dev-dependencies shared across bench/test targets")]

mod common;

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use localhold::{
    config::{LimitsConfig, SearchConfig},
    embedding::NoopEmbedding,
    engine::LocalHoldEngine,
    store::SqliteStore,
    types::Memory,
};

use crate::common::seeder::BenchSeeder;

fn make_engine() -> LocalHoldEngine<SqliteStore> {
    let store = SqliteStore::in_memory().expect("in-memory store");
    let embedding: Arc<dyn localhold::embedding::EmbeddingProvider> = Arc::new(NoopEmbedding::new());
    LocalHoldEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default())
}

async fn run_batch_store(engine: &LocalHoldEngine<SqliteStore>, memories: Vec<Memory>) {
    let supersedes = vec![None; memories.len()];
    let _ids = engine.batch_store(memories, supersedes).await.expect("batch store");
}

#[expect(unused_results, reason = "criterion bench_with_input returns a builder ref we do not chain")]
fn store_batch_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("store_batch");
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    for batch_size in [1_usize, 10_usize, 50_usize, 100_usize] {
        // Create a fresh engine for each benchmark parameter
        // to avoid accumulating rows that slow down subsequent inserts.
        let engine = make_engine();
        let mut seeder = BenchSeeder::new(99_u64);

        group.bench_with_input(BenchmarkId::from_parameter(batch_size), &batch_size, |b, &batch_size| {
            b.to_async(&rt).iter(|| {
                let memories = seeder.memories(batch_size);
                run_batch_store(&engine, memories)
            });
        });
    }

    group.finish();
}

criterion_group!(benches, store_batch_benchmark);
criterion_main!(benches);
