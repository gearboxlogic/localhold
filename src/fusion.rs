//! Reciprocal Rank Fusion (RRF) — merges ranked result lists from different
//! retrieval methods (e.g., semantic ANN + FTS5 keyword search).
//!
//! Formula: `score(d) = Σ(weight_r / (k + rank_r(d)))` where `k` is a constant
//! (default 60) that dampens the influence of high-ranking outliers.
//!
//! RRF is score-agnostic: it uses only rank positions, so BM25 scores and L2
//! distances never need normalization.

use std::collections::HashMap;

use crate::{
    ordering,
    types::{MemoryId, SearchMode, SearchResult},
};

/// A search result annotated with its RRF fusion score and which retrieval
/// paths contributed to it.
#[derive(Debug, Clone)]
pub(crate) struct FusedResult {
    pub result: SearchResult,
    pub rrf_score: f64,
    pub match_sources: Vec<SearchMode>,
}

/// Accumulate RRF scores from a single ranked result list into the shared
/// `entries` map. Each result contributes `weight / (k + rank + 1)` to its
/// document's RRF score, and `source` is recorded in the match sources.
#[expect(clippy::float_arithmetic, reason = "RRF scoring requires floating-point arithmetic by design")]
#[expect(clippy::as_conversions, reason = "rank usize to f64 cast: rank values are always small")]
#[expect(clippy::cast_precision_loss, reason = "rank usize to f64: values are small enough that precision loss is negligible")]
fn accumulate_rrf(entries: &mut HashMap<MemoryId, FusionEntry>, results: Vec<SearchResult>, k: f64, weight: f64, source: SearchMode) {
    for (rank, sr) in results.into_iter().enumerate() {
        let score = weight / (k + (rank as f64) + 1.0_f64);
        let id = sr.memory.id;
        let entry = entries.entry(id).or_insert_with(|| FusionEntry {
            result: sr,
            rrf_score: 0.0_f64,
            sources: Vec::new(),
        });
        entry.rrf_score += score;
        if !entry.sources.contains(&source) {
            entry.sources.push(source);
        }
    }
}

/// Merge two ranked result lists using Reciprocal Rank Fusion.
///
/// Both input lists are consumed (moved) to avoid cloning `SearchResult` values.
/// The returned list is sorted by RRF score (descending) and truncated to `limit`.
#[expect(clippy::too_many_arguments, reason = "RRF requires two result lists, k, two weights, and limit -- all semantically distinct")]
pub(crate) fn reciprocal_rank_fusion(
    semantic_results: Vec<SearchResult>,
    fts_results: Vec<SearchResult>,
    k: u32,
    semantic_weight: f64,
    fts_weight: f64,
    limit: usize,
) -> Vec<FusedResult> {
    let k_f64 = f64::from(k);

    let mut entries: HashMap<MemoryId, FusionEntry> = HashMap::with_capacity(semantic_results.len().saturating_add(fts_results.len()));

    accumulate_rrf(&mut entries, semantic_results, k_f64, semantic_weight, SearchMode::Semantic);
    accumulate_rrf(&mut entries, fts_results, k_f64, fts_weight, SearchMode::Keyword);

    let mut fused: Vec<FusedResult> = entries
        .into_values()
        .map(|e| FusedResult {
            result: e.result,
            rrf_score: e.rrf_score,
            match_sources: e.sources,
        })
        .collect();

    // Sort by RRF score descending (higher = more relevant), then ID for a total order.
    fused.sort_by(|a, b| ordering::cmp_f64_desc(a.rrf_score, b.rrf_score).then_with(|| b.result.memory.id.cmp(&a.result.memory.id)));
    fused.truncate(limit);
    fused
}

struct FusionEntry {
    result: SearchResult,
    rrf_score: f64,
    sources: Vec<SearchMode>,
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::types::{AccessPolicy, Memory, Provenance};

    fn make_result(id_suffix: u8) -> SearchResult {
        SearchResult {
            memory: Memory {
                id: MemoryId::new(),
                content: format!("content-{id_suffix}"),
                tags: Vec::new(),
                provenance: Provenance::default(),
                access_policy: AccessPolicy::Public,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                expires_at: None,
                has_embedding: false,
                memory_type: crate::types::MemoryType::default(),
                importance: crate::types::Importance::DEFAULT,
                confidence: crate::types::Confidence::DEFAULT,
                impression_count: 0,
                last_impressed_at: None,
                superseded_by: None,
                activity_mass: 0.0,
                last_used_at: None,
                entities: Vec::new(),
                was_redacted: false,
            },
            distance: Some(f64::from(id_suffix)),
            retrieval_score: None,
            reranker_score: None,
            composite_score: None,
            score_breakdown: None,
        }
    }

    fn make_result_with_id(id: MemoryId) -> SearchResult {
        let mut result = make_result(0);
        result.memory.id = id;
        result
    }

    #[test]
    fn empty_inputs_return_empty() {
        let fused = reciprocal_rank_fusion(vec![], vec![], 60, 1.0, 1.0, 10);
        assert!(fused.is_empty());
    }

    #[test]
    fn single_list_semantic_only() {
        let sem = vec![make_result(1), make_result(2)];
        let fused = reciprocal_rank_fusion(sem, vec![], 60, 1.0, 1.0, 10);
        assert_eq!(fused.len(), 2);
        assert!(fused[0].rrf_score > fused[1].rrf_score);
        assert_eq!(fused[0].match_sources, vec![SearchMode::Semantic]);
    }

    #[test]
    fn single_list_fts_only() {
        let fts = vec![make_result(1), make_result(2)];
        let fused = reciprocal_rank_fusion(vec![], fts, 60, 1.0, 1.0, 10);
        assert_eq!(fused.len(), 2);
        assert_eq!(fused[0].match_sources, vec![SearchMode::Keyword]);
    }

    #[test]
    fn overlapping_results_rank_higher() {
        let shared = make_result(1);
        let sem_only = make_result(2);
        let fts_only = make_result(3);

        let sem = vec![shared.clone(), sem_only];
        let fts = vec![shared, fts_only];

        let fused = reciprocal_rank_fusion(sem, fts, 60, 1.0, 1.0, 10);
        assert_eq!(fused.len(), 3);
        // The shared result should be ranked first due to contributions from both lists.
        assert_eq!(fused[0].match_sources.len(), 2);
        assert!(fused[0].rrf_score > fused[1].rrf_score);
    }

    #[test]
    fn limit_truncates() {
        let sem = vec![make_result(1), make_result(2), make_result(3)];
        let fused = reciprocal_rank_fusion(sem, vec![], 60, 1.0, 1.0, 2);
        assert_eq!(fused.len(), 2);
    }

    #[test]
    fn equal_rrf_scores_tie_break_by_id_descending() {
        let low_id: MemoryId = "01J0000000000000000000000A".parse().unwrap();
        let high_id: MemoryId = "01J0000000000000000000000B".parse().unwrap();

        let fused = reciprocal_rank_fusion(vec![make_result_with_id(low_id)], vec![make_result_with_id(high_id)], 60, 1.0, 1.0, 10);

        assert_eq!(fused[0].result.memory.id, high_id);
        assert_eq!(fused[1].result.memory.id, low_id);
    }

    #[test]
    fn zero_weight_disables_path() {
        let sem = vec![make_result(1)];
        let fts = vec![make_result(2)];
        let fused = reciprocal_rank_fusion(sem, fts, 60, 0.0, 1.0, 10);
        // Semantic weight is 0, so semantic result has score 0 and FTS result should rank first.
        assert_eq!(fused[0].match_sources, vec![SearchMode::Keyword]);
    }

    // -- RR-125: RRF dedup with duplicate MemoryId in one list ---------------

    #[test]
    fn duplicate_id_in_same_list_accumulates_score() {
        // Create two SearchResults with the same MemoryId in the semantic list.
        let r1 = make_result(1);
        let id = r1.memory.id;
        let r2 = SearchResult {
            memory: Memory {
                id, // same ID
                content: "content-1-dup".into(),
                tags: Vec::new(),
                provenance: Provenance::default(),
                access_policy: AccessPolicy::Public,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                expires_at: None,
                has_embedding: false,
                memory_type: crate::types::MemoryType::default(),
                importance: crate::types::Importance::DEFAULT,
                confidence: crate::types::Confidence::DEFAULT,
                impression_count: 0,
                last_impressed_at: None,
                superseded_by: None,
                activity_mass: 0.0,
                last_used_at: None,
                entities: Vec::new(),
                was_redacted: false,
            },
            distance: Some(2.0_f64),
            retrieval_score: None,
            reranker_score: None,
            composite_score: None,
            score_breakdown: None,
        };

        // Both entries share the same ID in the semantic list.
        let sem = vec![r1, r2];
        let fused = reciprocal_rank_fusion(sem, vec![], 60, 1.0, 1.0, 10);

        // The HashMap dedup means the same ID only appears once, with accumulated score.
        assert_eq!(fused.len(), 1, "duplicate IDs in same list should be merged into one entry");

        // Score should be sum of both rank contributions: 1/(60+1) + 1/(60+2)
        let expected_score = 1.0_f64 / 61.0_f64 + 1.0_f64 / 62.0_f64;
        let actual_score = fused[0].rrf_score;
        assert!(
            (actual_score - expected_score).abs() < 1e-10_f64,
            "accumulated score should be {expected_score}, got {actual_score}"
        );
    }

    #[test]
    fn duplicate_id_across_lists_records_both_sources() {
        // Same ID appears in both semantic and FTS lists.
        let shared = make_result(1);
        let sem = vec![shared.clone()];
        let fts = vec![shared];

        let fused = reciprocal_rank_fusion(sem, fts, 60, 1.0, 1.0, 10);
        assert_eq!(fused.len(), 1, "same ID across lists should produce one entry");
        assert_eq!(fused[0].match_sources.len(), 2, "should record both Semantic and Keyword sources");
        assert!(fused[0].match_sources.contains(&SearchMode::Semantic));
        assert!(fused[0].match_sources.contains(&SearchMode::Keyword));
    }
}
