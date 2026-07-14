//! Shared deterministic ordering helpers.

use std::cmp::Ordering;

use chrono::{DateTime, Utc};

use crate::types::{Memory, SearchResult};

/// Return the newest valid update timestamp for scoring and score tie-breaks.
///
/// Future `updated_at` values are invalid scoring inputs. Fall back to
/// `created_at` when it is not in the future; otherwise return `None` so the
/// caller can apply a neutral or minimum score.
pub(crate) fn valid_updated_at(memory: &Memory, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if memory.updated_at <= now {
        Some(memory.updated_at)
    } else if memory.created_at <= now {
        Some(memory.created_at)
    } else {
        None
    }
}

/// Return the last-use timestamp only when it is not in the future.
pub(crate) fn valid_last_used_at(memory: &Memory, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    memory.last_used_at.filter(|last_used_at| *last_used_at <= now)
}

/// Compare scores descending using total floating-point ordering.
pub(crate) fn cmp_f64_desc(a: f64, b: f64) -> Ordering {
    b.total_cmp(&a)
}

fn cmp_optional_datetime_desc(a: Option<DateTime<Utc>>, b: Option<DateTime<Utc>>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Composite score order: score descending, valid update timestamp descending,
/// then memory ID descending.
pub(crate) fn cmp_search_result_score_desc(a: &SearchResult, b: &SearchResult, now: DateTime<Utc>) -> Ordering {
    let a_score = a.composite_score.unwrap_or(f64::NEG_INFINITY);
    let b_score = b.composite_score.unwrap_or(f64::NEG_INFINITY);
    cmp_f64_desc(a_score, b_score)
        .then_with(|| cmp_optional_datetime_desc(valid_updated_at(&a.memory, now), valid_updated_at(&b.memory, now)))
        .then_with(|| b.memory.id.cmp(&a.memory.id))
}

/// Distance order: distance ascending, then memory ID ascending. Missing
/// distances sort after present distances.
pub(crate) fn cmp_search_result_distance_asc(a: &SearchResult, b: &SearchResult) -> Ordering {
    match (a.distance, b.distance) {
        (Some(a), Some(b)) => a.total_cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
    .then_with(|| a.memory.id.cmp(&b.memory.id))
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone as _};

    use super::*;
    use crate::types::{AccessPolicy, Confidence, Importance, MemoryId, MemoryType, Provenance};

    fn id(value: &str) -> MemoryId {
        value.parse().unwrap()
    }

    fn memory(id: MemoryId) -> Memory {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        Memory {
            id,
            content: "test".into(),
            tags: Vec::new(),
            provenance: Provenance::default(),
            access_policy: AccessPolicy::Public,
            created_at: now,
            updated_at: now,
            record_revision: 0_i64,
            expires_at: None,
            has_embedding: false,
            memory_type: MemoryType::default(),
            importance: Importance::DEFAULT,
            confidence: Confidence::DEFAULT,
            impression_count: 0,
            last_impressed_at: None,
            superseded_by: None,
            activity_mass: 0.0,
            last_used_at: None,
            entities: Vec::new(),
            was_redacted: false,
        }
    }

    fn result(id: MemoryId, distance: Option<f64>, score: Option<f64>) -> SearchResult {
        SearchResult {
            memory: memory(id),
            distance,
            retrieval_score: None,
            reranker_score: None,
            composite_score: score,
            score_breakdown: None,
        }
    }

    #[test]
    fn score_order_uses_valid_update_time_then_id() {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let mut older = result(id("01J0000000000000000000000A"), None, Some(42.0_f64));
        let mut newer = result(id("01J0000000000000000000000B"), None, Some(42.0_f64));
        older.memory.updated_at = now - Duration::hours(1);
        newer.memory.updated_at = now;

        assert_eq!(cmp_search_result_score_desc(&newer, &older, now), Ordering::Less);
    }

    #[test]
    fn score_order_ignores_future_update_time_for_tiebreak() {
        let now = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();
        let mut future = result(id("01J0000000000000000000000A"), None, Some(42.0_f64));
        let mut current = result(id("01J0000000000000000000000B"), None, Some(42.0_f64));
        future.memory.created_at = now - Duration::hours(2);
        future.memory.updated_at = now + Duration::hours(1);
        current.memory.created_at = now;
        current.memory.updated_at = now;

        assert_eq!(cmp_search_result_score_desc(&current, &future, now), Ordering::Less);
    }

    #[test]
    fn distance_order_ties_by_id_ascending() {
        let low_id = result(id("01J0000000000000000000000A"), Some(1.0_f64), None);
        let high_id = result(id("01J0000000000000000000000B"), Some(1.0_f64), None);

        assert_eq!(cmp_search_result_distance_asc(&low_id, &high_id), Ordering::Less);
    }
}
