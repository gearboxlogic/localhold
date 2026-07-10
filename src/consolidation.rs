//! Consolidation — duplicate group discovery via union-find clustering.
//!
//! Pure algorithmic functions with zero dependency on [`LocalHoldEngine`](crate::engine::LocalHoldEngine).
//! Operates on `&[MemoryWithEmbedding]` and produces [`ConsolidateResult`].

use std::collections::HashMap;

use crate::{store::MemoryWithEmbedding, types::MemoryId};

fn cmp_duplicate_representative(a: &crate::types::Memory, b: &crate::types::Memory) -> std::cmp::Ordering {
    let a_effective = a.last_used_at.unwrap_or(a.updated_at);
    let b_effective = b.last_used_at.unwrap_or(b.updated_at);
    a_effective.cmp(&b_effective).then_with(|| a.updated_at.cmp(&b.updated_at)).then_with(|| a.id.cmp(&b.id))
}

/// A pairwise similarity entry: `(index_a, index_b, cosine_similarity)`.
type PairSimilarity = (usize, usize, f64);

/// A group of near-duplicate memories.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub(crate) struct DuplicateGroup {
    /// The most recently accessed memory in the group (kept as the canonical version).
    pub representative_id: MemoryId,
    /// All memory IDs in the group, including the representative.
    pub member_ids: Vec<MemoryId>,
    /// Average pairwise similarity within the group.
    pub similarity: f64,
    /// Number of members in the group.
    pub member_count: usize,
}

/// Result of a consolidation operation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub(crate) struct ConsolidateResult {
    /// Groups of near-duplicate memories found.
    pub groups: Vec<DuplicateGroup>,
    /// Whether merging was performed (`false` when `dry_run` is `true`).
    pub merged: bool,
}

/// Compute cosine similarity between two vectors.
///
/// Returns a value in `[-1.0, 1.0]` where 1.0 means identical direction.
/// Returns 0.0 if either vector has zero magnitude.
///
/// Uses f32 accumulators throughout for auto-vectorization on 768-dim
/// embeddings. The inputs are L2-normalized, so f32 precision is sufficient.
#[expect(clippy::float_arithmetic, reason = "cosine similarity requires floating-point arithmetic")]
pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0_f64;
    }
    let mut dot = 0.0_f32;
    let mut norm_a = 0.0_f32;
    let mut norm_b = 0.0_f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < 1e-12_f32 {
        return 0.0_f64;
    }
    let result = dot / denom;
    if result.is_nan() {
        return 0.0_f64;
    }
    f64::from(result)
}

fn uf_find(parent: &mut [usize], i: usize) -> usize {
    let mut root = i;
    while parent[root] != root {
        // Path compression.
        parent[root] = parent[parent[root]];
        root = parent[root];
    }
    root
}

#[expect(clippy::arithmetic_side_effects, reason = "union-find rank increment cannot overflow for practical input sizes")]
fn uf_union(parent: &mut [usize], rank: &mut [usize], a: usize, b: usize) {
    let ra = uf_find(parent, a);
    let rb = uf_find(parent, b);
    if ra == rb {
        return;
    }
    match rank[ra].cmp(&rank[rb]) {
        std::cmp::Ordering::Less => parent[ra] = rb,
        std::cmp::Ordering::Greater => parent[rb] = ra,
        std::cmp::Ordering::Equal => {
            parent[rb] = ra;
            rank[ra] += 1;
        }
    }
}

/// Build a `DuplicateGroup` from a set of member indices.
#[expect(clippy::float_arithmetic, reason = "average similarity computation requires floating-point arithmetic")]
#[expect(clippy::as_conversions, reason = "usize index used for union-find — always small")]
#[expect(clippy::cast_precision_loss, reason = "count -> f64: group sizes are always small")]
fn build_duplicate_group(memories: &[MemoryWithEmbedding], members: &[usize], pair_similarities: &[PairSimilarity], parent: &mut [usize]) -> DuplicateGroup {
    // Representative: most recently used/updated memory.
    #[expect(clippy::expect_used, reason = "members is guaranteed non-empty by the filter(|_, members| members.len() >= 2) above")]
    let representative_idx = members
        .iter()
        .copied()
        .max_by(|&idx_a, &idx_b| cmp_duplicate_representative(&memories[idx_a].memory, &memories[idx_b].memory))
        .expect("members guaranteed non-empty");

    let mut member_ids: Vec<MemoryId> = members.iter().map(|&idx| memories[idx].memory.id).collect();
    member_ids.sort();
    let representative_id = memories[representative_idx].memory.id;

    // Re-derive average similarity for this group from stored pairs.
    let group_root = uf_find(parent, members[0]);
    let mut sim_sum = 0.0_f64;
    let mut sim_count = 0_usize;
    for &(pi, pj, sim) in pair_similarities {
        if uf_find(parent, pi) == group_root && uf_find(parent, pj) == group_root {
            sim_sum += sim;
            sim_count = sim_count.saturating_add(1);
        }
    }
    let avg_sim = if sim_count > 0 { sim_sum / sim_count as f64 } else { 0.0_f64 };

    DuplicateGroup {
        representative_id,
        member_ids,
        similarity: avg_sim,
        member_count: members.len(),
    }
}

/// A pre-computed neighbor pair discovered by ANN search.
#[derive(Debug, Clone)]
pub(crate) struct NeighborPair {
    pub id_a: MemoryId,
    pub id_b: MemoryId,
    pub similarity: f64,
}

/// Convert a cosine similarity threshold to the equivalent L2 distance threshold
/// for L2-normalized embeddings.
///
/// Relationship: `L2 = sqrt(2 - 2 * cosine)` for unit-length vectors.
pub(crate) fn cosine_to_l2_threshold(cosine_threshold: f64) -> f64 {
    // Clamp to valid cosine range to avoid NaN from sqrt of negative.
    let clamped = cosine_threshold.clamp(0.0_f64, 1.0_f64);
    clamped.mul_add(-2.0_f64, 2.0_f64).sqrt()
}

/// Convert an L2 distance to cosine similarity for L2-normalized embeddings.
#[expect(clippy::float_arithmetic, reason = "clamp on result of mul_add is required for numerical safety")]
pub(crate) fn l2_to_cosine(l2_distance: f64) -> f64 {
    (l2_distance * l2_distance).mul_add(-0.5_f64, 1.0_f64).clamp(0.0_f64, 1.0_f64)
}

/// Find groups of near-duplicate memories from pre-computed neighbor pairs.
///
/// This is the ANN-accelerated variant: instead of O(n²) brute-force pairwise
/// comparison, it accepts sparse pairs discovered by ANN index lookups, achieving
/// O(n log n) overall.
///
/// The `memories` slice provides metadata for representative selection and group
/// construction. The `pairs` provide the edge list for union-find clustering.
pub(crate) fn find_duplicate_groups_from_pairs(memories: &[MemoryWithEmbedding], pairs: &[NeighborPair], max_groups: usize) -> Vec<DuplicateGroup> {
    let n = memories.len();
    if n < 2 || pairs.is_empty() {
        return Vec::new();
    }

    // Build ID → index mapping for union-find.
    let id_to_idx: HashMap<MemoryId, usize> = memories.iter().enumerate().map(|(i, m)| (m.memory.id, i)).collect();

    let mut parent: Vec<usize> = (0..n).collect();
    let mut rank: Vec<usize> = vec![0; n];
    let mut pair_similarities: Vec<PairSimilarity> = Vec::new();

    for pair in pairs {
        let Some(&i) = id_to_idx.get(&pair.id_a) else { continue };
        let Some(&j) = id_to_idx.get(&pair.id_b) else { continue };
        if i == j {
            continue;
        }
        uf_union(&mut parent, &mut rank, i, j);
        let (lo, hi) = if i < j { (i, j) } else { (j, i) };
        pair_similarities.push((lo, hi, pair.similarity));
    }

    // Collect groups from union-find.
    let mut group_members: HashMap<usize, Vec<usize>> = HashMap::new();
    for i in 0..n {
        let root = uf_find(&mut parent, i);
        group_members.entry(root).or_default().push(i);
    }

    let mut groups: Vec<DuplicateGroup> = group_members
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .map(|(_, members)| build_duplicate_group(memories, &members, &pair_similarities, &mut parent))
        .collect();

    groups.sort_by(|a, b| {
        b.member_count
            .cmp(&a.member_count)
            .then_with(|| b.similarity.total_cmp(&a.similarity))
            .then_with(|| b.representative_id.cmp(&a.representative_id))
    });
    groups.truncate(max_groups);
    groups
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use chrono::{TimeZone as _, Utc};

    use super::*;
    use crate::types::{AccessPolicy, Memory, MemoryId, MemoryType, Provenance};

    /// Build a minimal `MemoryWithEmbedding` for consolidation tests.
    fn make_mem_with_embedding(embedding: Option<Vec<f32>>) -> MemoryWithEmbedding {
        MemoryWithEmbedding {
            memory: Memory {
                id: MemoryId::new(),
                content: "test".into(),
                tags: vec![],
                provenance: Provenance::default(),
                access_policy: AccessPolicy::Public,
                created_at: Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap(),
                updated_at: Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap(),
                expires_at: None,
                has_embedding: embedding.is_some(),
                memory_type: MemoryType::default(),
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
            embedding,
        }
    }

    fn fixed_id(value: &str) -> MemoryId {
        value.parse().unwrap()
    }

    // -- RR-008: cosine_similarity -------------------------------------------

    #[test]
    fn cosine_similarity_identical_vectors() {
        let v = vec![1.0_f32, 0.0, 0.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0_f64).abs() < 1e-6_f64, "identical vectors should have similarity ~1.0, got {sim}");
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![0.0_f32, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6_f64, "orthogonal vectors should have similarity ~0.0, got {sim}");
    }

    #[test]
    fn cosine_similarity_mismatched_lengths() {
        let a = vec![1.0_f32, 0.0];
        let b = vec![1.0_f32, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.0_f64).abs() < f64::EPSILON, "mismatched lengths should return 0.0");
    }

    #[test]
    fn cosine_similarity_empty_vectors() {
        let sim = cosine_similarity(&[], &[]);
        assert!((sim - 0.0_f64).abs() < f64::EPSILON, "empty vectors should return 0.0");
    }

    #[test]
    fn cosine_similarity_zero_magnitude_vector() {
        let a = vec![0.0_f32, 0.0, 0.0];
        let b = vec![1.0_f32, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 0.0_f64).abs() < f64::EPSILON, "zero-magnitude vector should return 0.0");
    }

    // -- RR-011: find_duplicate_groups_from_pairs / union-find ----------------

    #[test]
    fn find_duplicate_groups_two_identical_embeddings() {
        let emb = vec![1.0_f32, 0.0, 0.0];
        let memories = vec![make_mem_with_embedding(Some(emb.clone())), make_mem_with_embedding(Some(emb))];
        let pairs = vec![NeighborPair {
            id_a: memories[0].memory.id,
            id_b: memories[1].memory.id,
            similarity: 1.0,
        }];
        let groups = find_duplicate_groups_from_pairs(&memories, &pairs, 10);
        assert_eq!(groups.len(), 1, "two identical embeddings should form 1 group");
        assert_eq!(groups[0].member_count, 2);
    }

    #[test]
    fn duplicate_representative_ties_by_updated_at_then_id() {
        let emb = vec![1.0_f32, 0.0, 0.0];
        let mut older_high_id = make_mem_with_embedding(Some(emb.clone()));
        older_high_id.memory.id = fixed_id("01J0000000000000000000000B");
        older_high_id.memory.updated_at = Utc.with_ymd_and_hms(2025, 6, 15, 11, 0, 0).unwrap();

        let mut newer_low_id = make_mem_with_embedding(Some(emb));
        newer_low_id.memory.id = fixed_id("01J0000000000000000000000A");
        newer_low_id.memory.updated_at = Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap();

        let memories = vec![older_high_id, newer_low_id];
        let pairs = vec![NeighborPair {
            id_a: memories[0].memory.id,
            id_b: memories[1].memory.id,
            similarity: 1.0,
        }];

        let groups = find_duplicate_groups_from_pairs(&memories, &pairs, 10);

        assert_eq!(groups[0].representative_id, fixed_id("01J0000000000000000000000A"));
    }

    #[test]
    fn duplicate_group_order_ties_by_similarity_then_representative_id() {
        let emb = vec![1.0_f32, 0.0, 0.0];
        let mut a = make_mem_with_embedding(Some(emb.clone()));
        a.memory.id = fixed_id("01J0000000000000000000000A");
        let mut b = make_mem_with_embedding(Some(emb.clone()));
        b.memory.id = fixed_id("01J0000000000000000000000B");
        let mut c = make_mem_with_embedding(Some(emb.clone()));
        c.memory.id = fixed_id("01J0000000000000000000000C");
        let mut d = make_mem_with_embedding(Some(emb));
        d.memory.id = fixed_id("01J0000000000000000000000D");

        let memories = vec![a, b, c, d];
        let pairs = vec![
            NeighborPair {
                id_a: fixed_id("01J0000000000000000000000A"),
                id_b: fixed_id("01J0000000000000000000000B"),
                similarity: 0.91,
            },
            NeighborPair {
                id_a: fixed_id("01J0000000000000000000000C"),
                id_b: fixed_id("01J0000000000000000000000D"),
                similarity: 0.99,
            },
        ];

        let groups = find_duplicate_groups_from_pairs(&memories, &pairs, 10);

        assert_eq!(groups[0].representative_id, fixed_id("01J0000000000000000000000D"));
        assert_eq!(groups[1].representative_id, fixed_id("01J0000000000000000000000B"));
    }

    #[test]
    fn find_duplicate_groups_two_dissimilar_embeddings() {
        let a = vec![1.0_f32, 0.0, 0.0];
        let b = vec![0.0_f32, 1.0, 0.0];
        let memories = vec![make_mem_with_embedding(Some(a)), make_mem_with_embedding(Some(b))];
        // No pairs — dissimilar embeddings produce no ANN matches.
        let groups = find_duplicate_groups_from_pairs(&memories, &[], 10);
        assert!(groups.is_empty(), "dissimilar embeddings should form 0 groups");
    }

    #[test]
    fn find_duplicate_groups_single_memory() {
        let emb = vec![1.0_f32, 0.0, 0.0];
        let memories = vec![make_mem_with_embedding(Some(emb))];
        let groups = find_duplicate_groups_from_pairs(&memories, &[], 10);
        assert!(groups.is_empty(), "single memory should form 0 groups");
    }

    #[test]
    fn find_duplicate_groups_none_embeddings_skipped() {
        let memories = vec![make_mem_with_embedding(None), make_mem_with_embedding(None)];
        let groups = find_duplicate_groups_from_pairs(&memories, &[], 10);
        assert!(groups.is_empty(), "memories without embeddings should be skipped");
    }

    // -- RR-122: uf_find and uf_union tests ----------------------------------

    #[test]
    fn uf_find_singleton_returns_self() {
        let mut parent: Vec<usize> = (0..5).collect();
        assert_eq!(uf_find(&mut parent, 3), 3);
    }

    #[test]
    fn uf_find_path_compression() {
        // Build a chain: 0 -> 1 -> 2 -> 3 (root)
        let mut parent = vec![1, 2, 3, 3, 4];
        let root = uf_find(&mut parent, 0);
        assert_eq!(root, 3);
        // After path compression, 0 should point closer to root.
        // Path halving: 0 -> 2 -> 3 (at minimum), then 0 -> 3 on next call.
        let root_again = uf_find(&mut parent, 0);
        assert_eq!(root_again, 3);
        // Verify compression happened (0 should point to 3 now).
        assert_eq!(parent[0], 3);
    }

    #[test]
    fn uf_union_merges_disjoint_sets() {
        let mut parent: Vec<usize> = (0..4).collect();
        let mut rank = vec![0_usize; 4];

        uf_union(&mut parent, &mut rank, 0, 1);
        assert_eq!(uf_find(&mut parent, 0), uf_find(&mut parent, 1));

        uf_union(&mut parent, &mut rank, 2, 3);
        assert_eq!(uf_find(&mut parent, 2), uf_find(&mut parent, 3));

        // 0-1 and 2-3 are different sets.
        assert_ne!(uf_find(&mut parent, 0), uf_find(&mut parent, 2));
    }

    #[test]
    fn uf_union_same_set_is_noop() {
        let mut parent: Vec<usize> = (0..3).collect();
        let mut rank = vec![0_usize; 3];

        uf_union(&mut parent, &mut rank, 0, 1);
        let parent_before = parent.clone();
        let rank_before = rank.clone();

        // Union again — should be a no-op since they share the same root.
        uf_union(&mut parent, &mut rank, 0, 1);
        assert_eq!(parent, parent_before);
        assert_eq!(rank, rank_before);
    }

    #[test]
    fn uf_union_rank_based_higher_rank_becomes_root() {
        let mut parent: Vec<usize> = (0..4).collect();
        let mut rank = vec![0_usize; 4];

        // Create a set with rank 1: union(0,1) => root=0, rank[0]=1
        uf_union(&mut parent, &mut rank, 0, 1);
        let root_01 = uf_find(&mut parent, 0);
        assert_eq!(rank[root_01], 1);

        // Now union with a rank-0 singleton (2). The higher-rank root should remain root.
        uf_union(&mut parent, &mut rank, 0, 2);
        let root_all = uf_find(&mut parent, 2);
        assert_eq!(root_all, root_01, "higher-rank root should remain the root");
    }

    #[test]
    fn uf_union_equal_rank_increments() {
        let mut parent: Vec<usize> = (0..4).collect();
        let mut rank = vec![0_usize; 4];

        // Two rank-0 singletons merged: rank of the resulting root should be 1.
        uf_union(&mut parent, &mut rank, 0, 1);
        let root = uf_find(&mut parent, 0);
        assert_eq!(rank[root], 1, "equal-rank union should increment rank by 1");
    }

    #[test]
    fn uf_transitivity_via_chain_union() {
        let mut parent: Vec<usize> = (0..5).collect();
        let mut rank = vec![0_usize; 5];

        uf_union(&mut parent, &mut rank, 0, 1);
        uf_union(&mut parent, &mut rank, 1, 2);
        uf_union(&mut parent, &mut rank, 2, 3);
        uf_union(&mut parent, &mut rank, 3, 4);

        // All elements should share the same root via transitive union.
        let root = uf_find(&mut parent, 0);
        for i in 1..5 {
            assert_eq!(uf_find(&mut parent, i), root, "element {i} should share root with 0");
        }
    }
}
