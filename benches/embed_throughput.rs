#![expect(missing_docs, reason = "benchmark binary — no public API to document")]
#![expect(clippy::expect_used, reason = "benchmarks use expect for infallible setup")]
#![expect(unused_crate_dependencies, reason = "dev-dependencies shared across bench/test targets")]

mod common;

use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use localhold::{
    config::{LimitsConfig, SearchConfig},
    engine::RecallEngine,
    store::SqliteStore,
    types::Memory,
};

use crate::common::seeder::BenchSeeder;

// ---------------------------------------------------------------------------
// Deterministic embedding provider (local to this benchmark)
// ---------------------------------------------------------------------------

/// Deterministic embedding provider for benchmarks.
///
/// Uses FNV-1a hash of the input text to seed a 768-dim vector, so identical
/// text produces identical embeddings. This avoids network calls while
/// exercising the full store+embed pipeline.
struct DeterministicEmbedding;

impl localhold::embedding::EmbeddingProvider for DeterministicEmbedding {
    fn embed<'a>(&'a self, text: &'a str) -> localhold::embedding::BoxFuture<'a, Result<Vec<f32>, localhold::error::EmbeddingError>> {
        Box::pin(async move { Ok(deterministic_embed(text)) })
    }

    fn health_check(&self) -> localhold::embedding::BoxFuture<'_, Result<(), localhold::error::EmbeddingError>> {
        Box::pin(async { Ok(()) })
    }
}

/// Generate a deterministic 768-dim embedding from text using FNV-1a hashing.
#[expect(clippy::float_arithmetic, reason = "intentional float math for deterministic benchmark embedding generation")]
fn deterministic_embed(text: &str) -> Vec<f32> {
    let mut embedding = vec![0.0_f32; 768];
    let hash = fnv1a(text);
    for (i, val) in embedding.iter_mut().enumerate() {
        #[expect(clippy::as_conversions, reason = "usize index always fits in u64")]
        let seed = hash.wrapping_add(i as u64);
        #[expect(clippy::as_conversions, reason = "intentional u64->f32 cast for deterministic embedding seed")]
        #[expect(clippy::cast_precision_loss, reason = "intentional u64->f32 cast for deterministic embedding seed")]
        #[expect(clippy::integer_division_remainder_used, reason = "intentional modular arithmetic for hash-based embedding seed")]
        {
            *val = ((seed % 20_000) as f32 / 10_000.0) - 1.0;
        }
    }
    // Normalize to unit length
    let norm = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for val in &mut embedding {
            *val /= norm;
        }
    }
    embedding
}

fn fnv1a(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in s.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// Benchmark
// ---------------------------------------------------------------------------

/// Build an engine with `DeterministicEmbedding` to measure the full
/// store-then-embed pipeline overhead (without network).
fn make_engine() -> RecallEngine<SqliteStore> {
    let store = SqliteStore::in_memory().expect("in-memory store");
    let embedding: Arc<dyn localhold::embedding::EmbeddingProvider> = Arc::new(DeterministicEmbedding);
    RecallEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default())
}

async fn run_store_and_embed(engine: &RecallEngine<SqliteStore>, memories: Vec<Memory>) {
    let _ids = engine.batch_store(memories, vec![]).await.expect("batch store");
    // Wait for background embedding tasks to complete so we
    // measure the full pipeline cost.
    engine.shutdown_for_test(std::time::Duration::from_secs(30_u64)).await;
}

#[expect(unused_results, reason = "criterion bench_with_input returns a builder ref we do not chain")]
fn embed_throughput_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("embed_throughput");
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    for count in [1_usize, 10_usize, 50_usize] {
        let engine = make_engine();
        let mut seeder = BenchSeeder::new(77_u64);

        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.to_async(&rt).iter(|| {
                let memories = seeder.memories(count);
                run_store_and_embed(&engine, memories)
            });
        });
    }

    group.finish();
}

criterion_group!(benches, embed_throughput_benchmark);
criterion_main!(benches);
