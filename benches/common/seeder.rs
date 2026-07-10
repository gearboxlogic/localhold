use localhold::types::{AccessPolicy, Memory, Provenance};
use rand::{prelude::*, rngs::StdRng};

/// Reproducible data generator for benchmarks.
///
/// Uses a seeded RNG so benchmark runs produce identical data,
/// making results comparable across runs.
pub(crate) struct BenchSeeder {
    rng: StdRng,
}

/// Word pool for generating random content and tags.
const WORDS: &[&str] = &[
    "memory",
    "recall",
    "search",
    "vector",
    "embedding",
    "semantic",
    "store",
    "agent",
    "context",
    "query",
    "filter",
    "index",
    "batch",
    "async",
    "rust",
    "sqlite",
    "model",
    "neural",
    "token",
    "encode",
    "decode",
    "latency",
    "throughput",
    "pipeline",
    "system",
    "design",
    "pattern",
    "module",
    "trait",
    "struct",
    "function",
    "method",
    "result",
    "error",
    "handle",
    "process",
    "compute",
    "optimize",
    "cache",
    "buffer",
    "stream",
    "channel",
    "thread",
    "runtime",
    "schedule",
    "dispatch",
    "allocate",
    "release",
    "transform",
];

/// Tag pool for generating random tags.
const TAG_POOL: &[&str] = &[
    "project",
    "bugfix",
    "feature",
    "docs",
    "refactor",
    "test",
    "config",
    "deploy",
    "review",
    "urgent",
    "low-priority",
    "backend",
    "frontend",
    "api",
    "database",
    "security",
    "performance",
    "monitoring",
    "logging",
    "infrastructure",
];

impl BenchSeeder {
    /// Create a new seeder with the given seed for reproducibility.
    pub(crate) fn new(seed: u64) -> Self {
        Self { rng: StdRng::seed_from_u64(seed) }
    }

    /// Generate a single random memory.
    pub(crate) fn memory(&mut self) -> Memory {
        let content = self.content();
        let tag_count = self.rng.random_range(0_usize..5_usize);
        let tags = self.tags(tag_count);

        let provenance = Provenance::new_for_test(Some("bench-agent".to_owned()), Some("bench-conv".to_owned()), Some("bench-conv".to_owned()));

        Memory::new_for_test(content, tags, provenance, AccessPolicy::Public)
    }

    /// Generate a batch of random memories.
    pub(crate) fn memories(&mut self, count: usize) -> Vec<Memory> {
        std::iter::repeat_with(|| self.memory()).take(count).collect()
    }

    /// Generate random content string (roughly 50-500 chars).
    fn content(&mut self) -> String {
        let word_count = self.rng.random_range(10_usize..80_usize);
        let mut parts = Vec::with_capacity(word_count);
        for _ in 0..word_count {
            let idx = self.rng.random_range(0_usize..WORDS.len());
            parts.push(WORDS[idx]);
        }
        parts.join(" ")
    }

    /// Generate random tags from the tag pool.
    fn tags(&mut self, count: usize) -> Vec<String> {
        let mut selected = Vec::with_capacity(count);
        for _ in 0..count {
            let idx = self.rng.random_range(0_usize..TAG_POOL.len());
            let tag = TAG_POOL[idx].to_owned();
            if !selected.contains(&tag) {
                selected.push(tag);
            }
        }
        selected
    }
}
