#![expect(missing_docs, reason = "benchmark binary — no public API to document")]
#![expect(clippy::expect_used, reason = "benchmarks use expect for infallible setup")]
#![expect(unused_crate_dependencies, reason = "dev-dependencies shared across bench/test targets")]

mod common;

use std::{hint::black_box, sync::Arc};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use localhold::{
    config::{LimitsConfig, SearchConfig},
    context::{ContextAuditDraft, ContextCreateDraft, ContextId, ContextKind},
    embedding::NoopEmbedding,
    engine::LocalHoldEngine,
    store::{ContextWriter as _, MemoryReader as _, MemoryWriter as _, SqliteStore},
    types::{MemoryFilter, QueryContext},
};

use crate::common::seeder::BenchSeeder;

#[expect(clippy::float_arithmetic, reason = "benchmark setup intentionally generates and normalizes synthetic vectors")]
fn benchmark_embedding(ordinal: usize) -> Vec<f32> {
    let mut state = u64::try_from(ordinal)
        .expect("benchmark ordinal fits u64")
        .wrapping_add(1)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut embedding = Vec::with_capacity(768);
    for _ in 0_usize..768_usize {
        state ^= state << 13_u32;
        state ^= state >> 7_u32;
        state ^= state << 17_u32;
        let sample = u16::try_from(state >> 48_u32).expect("shifted sample fits u16");
        embedding.push((f32::from(sample) / f32::from(u16::MAX)).mul_add(2.0, -1.0));
    }
    let norm = embedding.iter().map(|value| value * value).sum::<f32>().sqrt();
    for value in &mut embedding {
        *value /= norm;
    }
    embedding
}

/// Seed a file-backed store with `count` memories and return the engine.
///
/// Uses `tempfile` for realistic I/O characteristics.
#[expect(unused_results, reason = "batch_store IDs are not needed during setup seeding")]
fn seeded_engine(count: usize, tmp_path: &std::path::Path) -> (LocalHoldEngine<SqliteStore>, Vec<ContextId>) {
    let store = SqliteStore::open(tmp_path, 768_usize).expect("open file-backed store");
    let embedding: Arc<dyn localhold::embedding::EmbeddingProvider> = Arc::new(NoopEmbedding::new());
    let engine = LocalHoldEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default());

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let mut projects = Vec::new();
    let mut domains = Vec::new();
    for (kind, prefix, count, target) in [
        (ContextKind::PROJECT, "project", 10_usize, &mut projects),
        (ContextKind::DOMAIN, "domain", 6_usize, &mut domains),
    ] {
        for index in 0..count {
            let id = ContextId::new();
            let context_kind = ContextKind::new(kind).expect("benchmark context kind");
            rt.block_on(engine.store().create_context(
                &ContextCreateDraft::private(id, context_kind, format!("{prefix}/bench-{index}"), format!("{prefix} {index}"), "bench-agent"),
                &ContextAuditDraft::new("bench-agent", "benchmark_context_created").with_context(id),
            ))
            .expect("create benchmark context");
            target.push(id);
        }
    }
    let mut seeder = BenchSeeder::new(42_u64);
    let memories = seeder.memories(count);
    let mut selected_multi_kind_memberships = 0_usize;

    // Group deterministic setup so membership assignment keeps stable ordinals.
    for (chunk_index, chunk) in memories.chunks(100_usize).enumerate() {
        let ids = chunk
            .iter()
            .cloned()
            .enumerate()
            .map(|(item_index, memory)| {
                let ordinal = chunk_index.saturating_mul(100).saturating_add(item_index);
                let embedding = benchmark_embedding(ordinal);
                rt.block_on(engine.store().store(&memory, Some(&embedding))).expect("store benchmark memory")
            })
            .collect::<Vec<_>>();
        for (item_index, id) in ids.into_iter().enumerate() {
            let ordinal = chunk_index.saturating_mul(100).saturating_add(item_index);
            let project_index = ordinal.checked_rem(projects.len()).unwrap_or_default();
            let mut memberships = vec![projects[project_index]];
            if ordinal.checked_rem(3) != Some(0) {
                let domain_index = ordinal.checked_div(3).unwrap_or_default().checked_rem(domains.len()).unwrap_or_default();
                memberships.push(domains[domain_index]);
            }
            selected_multi_kind_memberships = selected_multi_kind_memberships.saturating_add(usize::from(memberships.as_slice() == [projects[0], domains[0]]));
            rt.block_on(engine.store().replace_memory_contexts(
                &id,
                &memberships,
                "bench-agent",
                &ContextAuditDraft::new("bench-agent", "benchmark_memberships").with_context(memberships[0]),
            ))
            .expect("assign benchmark contexts");
        }
    }
    assert!(selected_multi_kind_memberships > 0, "benchmark fixture must contain project[0] + domain[0] memories");

    (engine, vec![projects[0], domains[0]])
}

async fn run_search(store: &SqliteStore, context_ids: Option<Vec<ContextId>>) {
    let mut filter = MemoryFilter::default();
    filter.context_ids = context_ids;
    let ctx = QueryContext::default();
    let results = store.search_by_text("memory recall search", 10_usize, &filter, &ctx).await.expect("benchmark text search");
    let _results = black_box(results);
}

async fn run_semantic_search(store: &SqliteStore, context_ids: Option<Vec<ContextId>>) {
    let mut filter = MemoryFilter::default();
    filter.context_ids = context_ids;
    let ctx = QueryContext::default();
    let query = benchmark_embedding(usize::MAX.saturating_sub(7));
    let results = store.search_by_embedding(&query, 10_usize, &filter, &ctx, None).await.expect("benchmark semantic search");
    let _results = black_box(results);
}

#[expect(unused_results, reason = "criterion bench_with_input returns a builder ref we do not chain")]
fn search_latency_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_latency");
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    for count in [100_usize, 1_000_usize, 5_000_usize] {
        let tmp = tempfile::NamedTempFile::new().expect("temp file");
        let (engine, selected) = seeded_engine(count, tmp.path());
        let store = engine.store().clone();

        group.bench_with_input(BenchmarkId::new("unfiltered_text_baseline", count), &store, |b, store| {
            b.to_async(&rt).iter(|| run_search(store, None));
        });
        group.bench_with_input(BenchmarkId::new("broad_contexted", count), &store, |b, store| {
            b.to_async(&rt).iter(|| run_search(store, Some(Vec::new())));
        });
        group.bench_with_input(BenchmarkId::new("project_plus_domain", count), &store, |b, store| {
            b.to_async(&rt).iter(|| run_search(store, Some(selected.clone())));
        });
        group.bench_with_input(BenchmarkId::new("unfiltered_semantic_baseline", count), &store, |b, store| {
            b.to_async(&rt).iter(|| run_semantic_search(store, None));
        });
        group.bench_with_input(BenchmarkId::new("broad_contexted_semantic", count), &store, |b, store| {
            b.to_async(&rt).iter(|| run_semantic_search(store, Some(Vec::new())));
        });
        group.bench_with_input(BenchmarkId::new("project_plus_domain_semantic", count), &store, |b, store| {
            b.to_async(&rt).iter(|| run_semantic_search(store, Some(selected.clone())));
        });
    }

    group.finish();
}

criterion_group!(benches, search_latency_benchmark);
criterion_main!(benches);
