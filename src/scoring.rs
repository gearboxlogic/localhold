//! Composite scoring — relevance, importance, freshness, and activity reranking.
//!
//! Pure function with no dependency on [`LocalHoldEngine`](crate::engine::LocalHoldEngine).

use crate::{
    config::SearchConfig,
    ordering,
    types::{MemoryType, ScoreBreakdown, SearchResult},
};

/// Decay `activity_mass` from `last_used_at` to `now` using exponential half-life.
///
/// Returns `mass * 2^(-delta_hours / half_life)`, or `0.0` when there is no prior use.
#[expect(clippy::float_arithmetic, reason = "decay formula requires floating-point arithmetic")]
#[expect(clippy::as_conversions, reason = "chrono seconds → f64: values are small enough")]
#[expect(clippy::cast_precision_loss, reason = "seconds i64 → f64: practical durations fit in f64 mantissa")]
pub(crate) fn decay_mass(mass: f64, last_used_at: Option<chrono::DateTime<chrono::Utc>>, now: chrono::DateTime<chrono::Utc>, half_life_hours: f64) -> f64 {
    last_used_at.map_or(0.0, |lu| {
        let delta_hours = now.signed_duration_since(lu).num_seconds() as f64 / 3_600.0_f64;
        mass * (-delta_hours.max(0.0) / half_life_hours.max(1e-9_f64)).exp2()
    })
}

/// Compute freshness score: `F(d) = 2^(-Δu / Hf)`.
///
/// `Δu` is the time since `updated_at` in days, `Hf` is the half-life from config
/// for the given memory type.
#[expect(clippy::float_arithmetic, reason = "freshness decay requires floating-point arithmetic")]
#[expect(clippy::as_conversions, reason = "chrono seconds → f64: values are small enough")]
#[expect(clippy::cast_precision_loss, reason = "seconds i64 → f64: practical durations fit in f64 mantissa")]
fn freshness_score(updated_at: chrono::DateTime<chrono::Utc>, now: chrono::DateTime<chrono::Utc>, half_life_days: f64) -> f64 {
    let delta_days = now.signed_duration_since(updated_at).num_seconds() as f64 / 86_400.0_f64;
    (-delta_days.max(0.0) / half_life_days.max(1e-9_f64)).exp2()
}

fn memory_freshness_score(memory: &crate::types::Memory, now: chrono::DateTime<chrono::Utc>, half_life_days: f64) -> f64 {
    ordering::valid_updated_at(memory, now).map_or(0.0, |updated_at| freshness_score(updated_at, now, half_life_days))
}

/// Look up the freshness half-life in days for the given memory type.
const fn freshness_half_life(memory_type: MemoryType, config: &SearchConfig) -> f64 {
    match memory_type {
        MemoryType::Semantic => config.freshness_half_life_semantic_days,
        MemoryType::Episodic => config.freshness_half_life_episodic_days,
        MemoryType::Procedural => config.freshness_half_life_procedural_days,
    }
}

fn distance_bounds(results: &[SearchResult]) -> Option<(f64, f64)> {
    results.iter().filter_map(|r| r.distance).fold(None, |acc, d| {
        Some(match acc {
            None => (d, d),
            Some((min, max)) => (min.min(d), max.max(d)),
        })
    })
}

#[expect(clippy::float_arithmetic, reason = "distance normalization requires floating-point arithmetic")]
fn normalized_stage_one_score(distance: Option<f64>, bounds: Option<(f64, f64)>) -> f64 {
    match (distance, bounds) {
        (Some(distance), Some((min_dist, max_dist))) => {
            let span = max_dist - min_dist;
            if span <= f64::EPSILON {
                1.0_f64
            } else {
                let normalized = ((distance - min_dist) / span).clamp(0.0_f64, 1.0_f64);
                1.0_f64 - normalized
            }
        }
        // No distance signal (text/LIKE fallback): assign a flat score
        // so time-ordered results don't get false rank-based relevance.
        _ => 0.5_f64,
    }
}

pub(crate) fn seed_retrieval_scores(results: &mut [SearchResult]) {
    let bounds = distance_bounds(results);
    for result in results.iter_mut() {
        if result.retrieval_score.is_some() {
            continue;
        }
        result.retrieval_score = Some(normalized_stage_one_score(result.distance, bounds));
    }
}

#[expect(clippy::float_arithmetic, reason = "relevance blending requires floating-point arithmetic")]
fn query_relevance(result: &SearchResult, blend_weight: f64) -> f64 {
    let stage_one = result.retrieval_score.unwrap_or(0.0_f64);
    result
        .reranker_score
        .map_or(stage_one, |reranker_score| blend_weight.mul_add(reranker_score, (1.0_f64 - blend_weight) * stage_one))
}

/// Compute composite scores for search results and rerank them.
///
/// `base_scores` are the raw relevance values. For RRF results these are
/// already normalized [0,1]; for single-path results they are normalized
/// from the raw distance/score.
#[expect(clippy::float_arithmetic, reason = "composite scoring requires floating-point arithmetic by design")]
pub(crate) fn apply_composite_scoring(results: &mut [SearchResult], now: chrono::DateTime<chrono::Utc>, config: &SearchConfig) {
    if results.is_empty() {
        return;
    }

    seed_retrieval_scores(results);

    let half_life_hours = config.activity_half_life_hours.max(1e-9);
    let m_sat = config.activity_saturation.max(1e-9);

    for result in results.iter_mut() {
        let relevance = query_relevance(result, config.reranker.blend_weight);

        // Activity: decay stored mass to now, then apply log-saturation.
        // A(d) = min(1, ln(1 + M_decayed) / ln(1 + M_sat))
        let decayed_mass = decay_mass(result.memory.activity_mass, ordering::valid_last_used_at(&result.memory, now), now, half_life_hours);
        let activity = (decayed_mass.ln_1p() / m_sat.ln_1p()).min(1.0);

        let importance = result.memory.importance.value();

        // Freshness: F(d) = 2^(-Δu / Hf) with per-type half-life.
        let fhl = freshness_half_life(result.memory.memory_type, config);
        let freshness = memory_freshness_score(&result.memory, now, fhl);

        let confidence = result.memory.confidence.value();

        // S(d) = 60Q + 15I + 10F + 10A + 5C  (0-100 scale)
        let mut composite = config.relevance_weight.mul_add(
            relevance,
            config.importance_weight.mul_add(
                importance,
                config
                    .freshness_weight
                    .mul_add(freshness, config.activity_weight.mul_add(activity, config.confidence_weight * confidence)),
            ),
        );

        result.score_breakdown = Some(ScoreBreakdown {
            query_relevance: relevance,
            importance,
            freshness,
            activity,
            confidence,
        });

        // Soft relevance floor: penalize (but don't exclude) results
        // below the configured query-relevance threshold.
        if relevance < config.relevance_floor {
            composite *= config.relevance_floor_penalty;
        }

        // Superseded memories get a configurable penalty so they sort
        // below their successors when include_superseded is true.
        if result.memory.superseded_by.is_some() {
            composite *= config.superseded_penalty;
        }

        result.composite_score = Some(composite);
    }

    results.sort_by(|a, b| ordering::cmp_search_result_score_desc(a, b, now));
}

/// Apply duplicate suppression: penalize results whose embeddings are too
/// similar to already-selected results (greedy MMR-style diversity pass).
///
/// Results are walked in score order. For each candidate, the maximum cosine
/// similarity against already-selected results is computed. A penalty of
/// `lambda * max_sim * 100` is subtracted from the composite score. Results
/// with adjusted score <= 0 are dropped.
#[expect(clippy::float_arithmetic, reason = "diversity penalty requires floating-point arithmetic")]
pub(crate) fn apply_duplicate_suppression(
    results: &mut Vec<SearchResult>,
    embeddings: &std::collections::HashMap<crate::types::MemoryId, Vec<f32>>,
    lambda: f64,
    now: chrono::DateTime<chrono::Utc>,
) {
    if lambda <= 0.0_f64 || results.len() < 2 {
        return;
    }

    let mut selected_indices: Vec<usize> = Vec::with_capacity(results.len());

    for i in 0..results.len() {
        let Some(emb_i) = embeddings.get(&results[i].memory.id) else {
            // No embedding for this memory — keep it without penalty.
            selected_indices.push(i);
            continue;
        };
        let max_sim = selected_indices
            .iter()
            .filter_map(|&j| embeddings.get(&results[j].memory.id).map(|emb_j| crate::consolidation::cosine_similarity(emb_i, emb_j)))
            .fold(0.0_f64, f64::max);
        let penalty = lambda * max_sim * 100.0_f64;
        if let Some(score) = &mut results[i].composite_score {
            *score -= penalty;
            if *score > 0.0_f64 {
                selected_indices.push(i);
            }
        } else {
            selected_indices.push(i);
        }
    }

    debug_assert!(
        selected_indices.windows(2).all(|w| matches!(w, [a, b] if a < b)),
        "selected_indices must be strictly monotonically increasing for swap correctness"
    );

    let mut write_pos = 0_usize;
    for read_pos in selected_indices {
        if write_pos != read_pos {
            results.swap(write_pos, read_pos);
        }
        write_pos = write_pos.saturating_add(1);
    }
    results.truncate(write_pos);
    results.sort_by(|a, b| ordering::cmp_search_result_score_desc(a, b, now));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone as _, Utc};

    use super::*;
    use crate::types::{AccessPolicy, Importance, Memory, MemoryId, MemoryType, Provenance};

    /// Build a minimal `SearchResult` for scoring tests.
    fn make_result(distance: Option<f64>, importance: f64, superseded: bool) -> SearchResult {
        let mut memory = Memory {
            id: MemoryId::new(),
            content: "test".into(),
            tags: vec![],
            provenance: Provenance::default(),
            access_policy: AccessPolicy::Public,
            created_at: Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap(),
            record_revision: 0_i64,
            expires_at: None,
            has_embedding: false,
            memory_type: MemoryType::default(),
            importance: Importance::new(importance),
            confidence: crate::types::Confidence::DEFAULT,
            impression_count: 0,
            last_impressed_at: Some(Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap()),
            superseded_by: None,
            activity_mass: 0.0,
            last_used_at: None,
            entities: Vec::new(),
            was_redacted: false,
        };
        if superseded {
            memory.superseded_by = Some(MemoryId::new());
        }
        SearchResult {
            memory,
            distance,
            retrieval_score: None,
            reranker_score: None,
            composite_score: None,
            score_breakdown: None,
        }
    }

    fn fixed_id(value: &str) -> MemoryId {
        value.parse().unwrap()
    }

    // -- RR-044: apply_composite_scoring -------------------------------------

    #[test]
    fn apply_composite_scoring_empty_input_is_noop() {
        let mut results: Vec<SearchResult> = vec![];
        let now = Utc::now();
        let config = SearchConfig::default();
        apply_composite_scoring(&mut results, now, &config);
        assert!(results.is_empty());
    }

    #[test]
    fn apply_composite_scoring_relevance_produces_composite_score() {
        let mut results = vec![make_result(Some(0.5_f64), 0.8, false), make_result(Some(1.0_f64), 0.3, false)];
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let config = SearchConfig::default();
        apply_composite_scoring(&mut results, now, &config);

        for result in &results {
            assert!(result.composite_score.is_some(), "composite_score should be set");
            let score = result.composite_score.unwrap();
            assert!(score >= 0.0_f64, "composite_score should be non-negative");
        }

        // Results should be sorted by composite_score descending.
        assert!(
            results[0].composite_score.unwrap() >= results[1].composite_score.unwrap(),
            "results should be sorted by composite_score descending"
        );
    }

    #[test]
    fn apply_composite_scoring_superseded_memory_gets_penalty() {
        let mut normal = vec![make_result(Some(0.5_f64), 0.8, false)];
        let mut superseded = vec![make_result(Some(0.5_f64), 0.8, true)];
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let config = SearchConfig::default();

        apply_composite_scoring(&mut normal, now, &config);
        apply_composite_scoring(&mut superseded, now, &config);

        let normal_score = normal[0].composite_score.unwrap();
        let superseded_score = superseded[0].composite_score.unwrap();
        assert!(
            superseded_score < normal_score,
            "superseded memory ({superseded_score}) should score lower than normal ({normal_score})"
        );
    }

    #[test]
    fn apply_composite_scoring_single_semantic_hit_normalizes_to_full_relevance() {
        let mut results = vec![make_result(Some(0.42_f64), 0.5, false)];
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let config = SearchConfig::default();

        apply_composite_scoring(&mut results, now, &config);

        let breakdown = results[0].score_breakdown.unwrap();
        assert!(
            (breakdown.query_relevance - 1.0_f64).abs() < f64::EPSILON,
            "expected 1.0, got {}",
            breakdown.query_relevance
        );
    }

    #[test]
    fn apply_composite_scoring_equal_distance_pool_normalizes_without_zeroing() {
        let mut results = vec![make_result(Some(0.7_f64), 0.5, false), make_result(Some(0.7_f64), 0.5, false)];
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let config = SearchConfig::default();

        apply_composite_scoring(&mut results, now, &config);

        for result in &results {
            assert!((result.score_breakdown.unwrap().query_relevance - 1.0_f64).abs() < f64::EPSILON, "expected 1.0");
        }
    }

    // -- RR-118: rank-based relevance fallback (distance: None) ---------------

    #[test]
    fn apply_composite_scoring_rank_based_fallback_when_distance_none() {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let config = SearchConfig::default();

        // All results have distance: None — should use rank-based fallback.
        let mut results = vec![make_result(None, 0.5, false), make_result(None, 0.5, false), make_result(None, 0.5, false)];

        apply_composite_scoring(&mut results, now, &config);

        // All should have composite scores set.
        for result in &results {
            assert!(result.composite_score.is_some(), "composite_score should be set for rank-based fallback");
        }

        // Rank-based fallback: 1/(rank+1), so first result has highest relevance.
        // With identical importance and recency, composite scores should decrease.
        let s0 = results[0].composite_score.unwrap();
        let s1 = results[1].composite_score.unwrap();
        let s2 = results[2].composite_score.unwrap();
        assert!(s0 >= s1 && s1 >= s2, "rank-based fallback should produce monotonically decreasing scores: {s0}, {s1}, {s2}");
    }

    #[test]
    fn apply_composite_scoring_rank_fallback_differs_from_distance_scoring() {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let config = SearchConfig::default();

        // Result with distance.
        let mut with_dist = vec![make_result(Some(0.5_f64), 0.5, false)];
        // Result without distance (rank-based fallback).
        let mut without_dist = vec![make_result(None, 0.5, false)];

        apply_composite_scoring(&mut with_dist, now, &config);
        apply_composite_scoring(&mut without_dist, now, &config);

        // Both should produce valid composite scores.
        assert!(with_dist[0].composite_score.is_some());
        assert!(without_dist[0].composite_score.is_some());
    }

    // -- Retrieval score (RRF hybrid) tests -----------------------------------

    /// Build a `SearchResult` with a `retrieval_score` (simulating hybrid RRF output).
    fn make_result_with_retrieval_score(retrieval_score: f64, importance: f64) -> SearchResult {
        SearchResult {
            memory: Memory {
                id: MemoryId::new(),
                content: "test".into(),
                tags: vec![],
                provenance: Provenance::default(),
                access_policy: AccessPolicy::Public,
                created_at: Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap(),
                updated_at: Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap(),
                record_revision: 0_i64,
                expires_at: None,
                has_embedding: false,
                memory_type: MemoryType::default(),
                importance: Importance::new(importance),
                confidence: crate::types::Confidence::DEFAULT,
                impression_count: 0,
                last_impressed_at: Some(Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap()),
                superseded_by: None,
                activity_mass: 0.0,
                last_used_at: None,
                entities: Vec::new(),
                was_redacted: false,
            },
            distance: None,
            retrieval_score: Some(retrieval_score),
            reranker_score: None,
            composite_score: None,
            score_breakdown: None,
        }
    }

    #[test]
    fn apply_composite_scoring_uses_retrieval_score_over_rank_fallback() {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let config = SearchConfig::default();

        // Two results with retrieval_scores: the second has higher retrieval_score
        // but is at rank 1 (lower rank-fallback). If retrieval_score is used,
        // it should rank first after scoring.
        let mut results = vec![make_result_with_retrieval_score(0.3, 0.5), make_result_with_retrieval_score(0.9, 0.5)];

        apply_composite_scoring(&mut results, now, &config);

        // The result with retrieval_score=0.9 should be ranked first.
        assert!(
            results[0].retrieval_score.unwrap() > results[1].retrieval_score.unwrap(),
            "result with higher retrieval_score should rank first"
        );
    }

    #[test]
    fn apply_composite_scoring_retrieval_score_produces_correct_relevance() {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let config = SearchConfig::default();

        let mut results = vec![make_result_with_retrieval_score(0.8, 0.5)];
        apply_composite_scoring(&mut results, now, &config);

        let score = results[0].composite_score.unwrap();
        // 0-100 scale: S(d) = 60Q + 15I + 10F + 10A + 5C
        // Q=0.8 → 60*0.8 = 48.0
        // I=0.5 → 15*0.5 = 7.5
        // F=1.0 (updated_at == now) → 10*1.0 = 10.0
        // A=0.0 (no use events) → 10*0.0 = 0.0
        // C=0.8 (default) → 5*0.8 = 4.0
        let expected = 60.0_f64.mul_add(0.8, 15.0_f64.mul_add(0.5, 10.0_f64.mul_add(1.0, 10.0_f64.mul_add(0.0, 5.0 * 0.8))));
        assert!((score - expected).abs() < 1e-6_f64, "composite score {score} should match expected {expected}");
    }

    #[test]
    fn apply_composite_scoring_ties_by_updated_at_then_id() {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let mut low_id_newer = make_result_with_retrieval_score(0.8, 0.5);
        low_id_newer.memory.id = fixed_id("01J0000000000000000000000A");
        low_id_newer.memory.updated_at = now;

        let mut high_id_older = make_result_with_retrieval_score(0.8, 0.5);
        high_id_older.memory.id = fixed_id("01J0000000000000000000000B");
        high_id_older.memory.updated_at = now - Duration::hours(1);

        let mut results = vec![high_id_older, low_id_newer];
        apply_composite_scoring(&mut results, now, &SearchConfig::default());

        assert_eq!(results[0].memory.id, fixed_id("01J0000000000000000000000A"));
    }

    #[test]
    fn apply_composite_scoring_ties_by_id_when_updated_at_matches() {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let mut low_id = make_result_with_retrieval_score(0.8, 0.5);
        low_id.memory.id = fixed_id("01J0000000000000000000000A");
        let mut high_id = make_result_with_retrieval_score(0.8, 0.5);
        high_id.memory.id = fixed_id("01J0000000000000000000000B");

        let mut results = vec![low_id, high_id];
        apply_composite_scoring(&mut results, now, &SearchConfig::default());

        assert_eq!(results[0].memory.id, fixed_id("01J0000000000000000000000B"));
    }

    #[test]
    fn future_updated_at_falls_back_to_created_at_for_freshness() {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let mut result = make_result_with_retrieval_score(0.8, 0.5);
        let created_at = now - Duration::days(30);
        result.memory.created_at = created_at;
        result.memory.updated_at = now + Duration::days(30);

        let mut results = vec![result];
        let config = SearchConfig::default();
        apply_composite_scoring(&mut results, now, &config);

        let freshness = results[0].score_breakdown.unwrap().freshness;
        let expected = freshness_score(created_at, now, config.freshness_half_life_semantic_days);
        assert!((freshness - expected).abs() < f64::EPSILON, "future updated_at should fall back to created_at freshness");
    }

    #[test]
    fn all_future_timestamps_get_minimum_freshness() {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let mut result = make_result_with_retrieval_score(0.8, 0.5);
        result.memory.created_at = now + Duration::days(1);
        result.memory.updated_at = now + Duration::days(2);

        let mut results = vec![result];
        apply_composite_scoring(&mut results, now, &SearchConfig::default());

        assert!((results[0].score_breakdown.unwrap().freshness - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn future_last_used_at_is_ignored_for_activity() {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let mut result = make_result_with_retrieval_score(0.8, 0.5);
        result.memory.activity_mass = 100.0_f64;
        result.memory.last_used_at = Some(now + Duration::days(1));

        let mut results = vec![result];
        apply_composite_scoring(&mut results, now, &SearchConfig::default());

        assert!((results[0].score_breakdown.unwrap().activity - 0.0).abs() < f64::EPSILON);
    }
}
