#![expect(missing_docs, reason = "benchmark binary — no public API to document")]
#![expect(clippy::expect_used, reason = "benchmarks use expect for infallible setup")]
#![expect(clippy::print_stdout, reason = "footprint benchmark reports results to stdout")]
#![expect(unused_crate_dependencies, reason = "dev-dependencies shared across bench/test targets")]

mod common;

use std::sync::Arc;

use localhold::{
    config::{LimitsConfig, SearchConfig},
    embedding::NoopEmbedding,
    engine::RecallEngine,
    store::SqliteStore,
    types::Memory,
};

use crate::common::seeder::BenchSeeder;

/// Read the current process RSS (Resident Set Size) in bytes from `/proc/self/statm`.
///
/// Returns `None` on non-Linux platforms or if `/proc` is unavailable.
fn rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let rss_pages: u64 = statm.split_whitespace().nth(1_usize)?.parse().ok()?;
    // Page size is typically 4096 on Linux
    rss_pages.checked_mul(4096_u64)
}

/// Format bytes as a human-readable string.
#[expect(clippy::as_conversions, reason = "u64-to-f64 cast for human-readable display")]
#[expect(clippy::cast_precision_loss, reason = "u64-to-f64 cast for human-readable display")]
#[expect(clippy::float_arithmetic, reason = "intentional float division for human-readable byte formatting")]
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_048_576_u64 {
        let mib = bytes as f64 / 1_048_576.0;
        format!("{mib:.1} MiB")
    } else if bytes >= 1024_u64 {
        let kib = bytes as f64 / 1024.0;
        format!("{kib:.1} KiB")
    } else {
        format!("{bytes} B")
    }
}

fn make_engine() -> RecallEngine<SqliteStore> {
    let store = SqliteStore::in_memory().expect("in-memory store");
    let embedding: Arc<dyn localhold::embedding::EmbeddingProvider> = Arc::new(NoopEmbedding::new());
    RecallEngine::new(store, embedding, LimitsConfig::default(), SearchConfig::default())
}

#[expect(unused_results, reason = "batch_store IDs are not needed during footprint seeding")]
async fn seed_memories(engine: &RecallEngine<SqliteStore>, memories: Vec<Memory>) {
    for chunk in memories.chunks(100_usize) {
        let batch: Vec<_> = chunk.to_vec();
        engine.batch_store(batch, vec![]).await.expect("batch store");
    }
}

fn print_row(count: usize, current_rss: Option<u64>, baseline_rss: Option<u64>) {
    match (current_rss, baseline_rss) {
        (Some(current), Some(baseline)) => {
            let delta = current.saturating_sub(baseline);
            println!("{count:<12} {:>15} {:>15}", format_bytes(current), format!("+ {}", format_bytes(delta)));
        }
        (Some(current), None) => {
            println!("{count:<12} {:>15} {:>15}", format_bytes(current), "n/a");
        }
        _ => {
            println!("{count:<12} {:>15} {:>15}", "unavailable", "n/a");
        }
    }
}

fn main() {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let counts = [0_usize, 100_usize, 1_000_usize, 5_000_usize, 10_000_usize];

    println!();
    println!("Memory Footprint Report");
    println!("{:-<50}", "");
    println!("{:<12} {:>15} {:>15}", "Memories", "RSS", "Delta");
    println!("{:-<50}", "");

    let baseline_rss = rss_bytes();

    for &count in &counts {
        let engine = make_engine();

        if count > 0_usize {
            let mut seeder = BenchSeeder::new(42_u64);
            let memories = seeder.memories(count);
            rt.block_on(seed_memories(&engine, memories));
        }

        print_row(count, rss_bytes(), baseline_rss);

        // Drop engine to release memory before next iteration
        drop(engine);
    }

    println!("{:-<50}", "");
    println!();
}
