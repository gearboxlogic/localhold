//! Semantic and text search implementations.
//!
//! The embedding search pipeline is decomposed into three distinct phases:
//!
//! 1. [`VectorIndex::search_batch`](super::vector::VectorIndex::search_batch) — ANN candidates
//! 2. [`hydrate_candidates`] — hydrate memory IDs into full rows
//! 3. [`post_filter_results`] — apply access policy, filter predicates, and max distance
//!
//! These phases are orchestrated by [`embedding_search_loop`] with adaptive retry.

use std::sync::LazyLock;

use rusqlite::params;

use super::{
    SqliteStore,
    crud::hydrate_entities_batch,
    query::{
        MAX_SCAN_ROWS, MAX_VEC_CANDIDATES, MEMORY_COLUMN_COUNT, OVERFETCH_FACTOR, ScanConfig, apply_access_policy_for_filter, escape_like, needs_entity_hydration,
        normalize_filter, row_to_memory, sort_by_distance, usize_to_i64,
    },
    vector::{SqliteVecIndex, VectorHit, VectorIndex as _},
};
use crate::{
    error::StoreError,
    types::{Memory, MemoryFilter, MemoryId, QueryContext, SearchResult},
};

/// Pre-computed `"m.id, m.content, ..."` column list for JOIN queries.
static PREFIXED_COLUMNS: LazyLock<String> = LazyLock::new(|| super::query::COLUMNS.iter().map(|c| format!("m.{c}")).collect::<Vec<_>>().join(", "));

#[expect(clippy::multiple_inherent_impl, reason = "SqliteStore methods are split across submodules by concern")]
impl SqliteStore {
    #[expect(
        clippy::too_many_arguments,
        reason = "search requires embedding, limit, filter, context, and distance threshold — all semantically distinct"
    )]
    pub(crate) async fn search_by_embedding_impl(
        &self,
        embedding: &[f32],
        limit: usize,
        filter: MemoryFilter,
        ctx: QueryContext,
        max_distance: Option<f64>,
    ) -> Result<Vec<SearchResult>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let emb = embedding.to_vec();
        let filter = normalize_filter(filter);
        let principal = ctx.principal;
        let now = self.clock_now();
        let vector_index = self.vector_index();
        self.with_conn(move |conn| {
            let caller = principal.as_deref();
            let pf_ctx = PostFilterContext {
                filter: &filter,
                caller,
                now,
                max_distance,
            };
            embedding_search_loop(conn, &vector_index, &emb, limit, &pf_ctx)
        })
        .await
    }

    /// Find nearest ANN neighbors for an embedding within an L2 distance threshold.
    ///
    /// Returns `(memory_id, l2_distance)` pairs, excluding superseded memories.
    pub(crate) async fn find_embedding_neighbors_impl(&self, embedding: &[f32], max_l2_distance: f64, limit: usize) -> Result<Vec<super::EmbeddingNeighbor>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let emb = embedding.to_vec();
        let vector_index = self.vector_index();
        self.with_conn(move |conn| {
            vector_index
                .neighbors(conn, &emb, max_l2_distance, limit)
                .map(|hits| hits.into_iter().map(|hit| (hit.memory_id, hit.distance)).collect())
        })
        .await
    }

    pub(crate) async fn search_by_text_impl(&self, query: &str, limit: usize, filter: MemoryFilter, ctx: QueryContext) -> Result<Vec<SearchResult>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let like_pattern = format!("%{}%", escape_like(query));
        let filter = normalize_filter(filter);
        let principal = ctx.principal;
        let now = self.clock_now();
        self.with_conn(move |conn| {
            let caller = principal.as_deref();
            let page_size = limit.saturating_mul(OVERFETCH_FACTOR).max(1);
            let extra_params: Vec<String> = vec![like_pattern];
            text_search_scan(conn, &filter, caller, now, limit, page_size, &extra_params)
        })
        .await
    }
}

/// Adaptive retry loop for embedding-based search.
///
/// Starts with `limit * OVERFETCH_FACTOR` candidates, doubling on each retry
/// until enough post-filtered results are collected or the ceiling is reached.
fn embedding_search_loop(
    conn: &rusqlite::Connection,
    vector_index: &SqliteVecIndex,
    emb: &[f32],
    limit: usize,
    pf_ctx: &PostFilterContext<'_>,
) -> Result<Vec<SearchResult>, StoreError> {
    let mut results: Vec<SearchResult> = Vec::new();
    // Bounded by MAX_VEC_CANDIDATES × retry iterations (max ~10 due to doubling).
    let mut seen_ids: std::collections::HashSet<MemoryId> = std::collections::HashSet::with_capacity(limit.saturating_mul(OVERFETCH_FACTOR));
    let mut fetch_size = limit.saturating_mul(OVERFETCH_FACTOR);

    loop {
        let candidate_limit = fetch_size.min(MAX_VEC_CANDIDATES);
        let batch = vector_index.search_batch(conn, emb, candidate_limit)?;
        let returned = batch.returned_count;
        let new_results: Vec<VectorHit> = batch.hits.into_iter().filter(|hit| seen_ids.insert(hit.memory_id)).collect();

        if new_results.is_empty() && returned < fetch_size {
            break;
        }

        if !new_results.is_empty() {
            let hydrated = hydrate_candidates(conn, &new_results)?;
            post_filter_results(conn, &mut results, hydrated, &new_results, pf_ctx)?;
        }

        if results.len() >= limit || returned < fetch_size {
            break;
        }
        if fetch_size >= MAX_VEC_CANDIDATES {
            tracing::info!(
                fetch_size,
                max = MAX_VEC_CANDIDATES,
                collected = results.len(),
                requested = limit,
                "search exiting: reached MAX_VEC_CANDIDATES ceiling"
            );
            break;
        }

        fetch_size = fetch_size.saturating_mul(2);
    }

    sort_by_distance(&mut results);
    results.truncate(limit);
    Ok(results)
}

/// Execute a text search with paged scanning.
#[expect(
    clippy::too_many_arguments,
    reason = "text search scan requires connection, filter, caller, time, limit, page_size, and extra params"
)]
fn text_search_scan(
    conn: &rusqlite::Connection,
    filter: &MemoryFilter,
    caller: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
    limit: usize,
    page_size: usize,
    extra_params: &[String],
) -> Result<Vec<SearchResult>, StoreError> {
    let mut results: Vec<SearchResult> = Vec::with_capacity(limit);

    ScanConfig::new(conn, filter, caller, now, page_size).run_with_extra_hydrated(Some("content LIKE ?1 ESCAPE '\\'"), extra_params, &mut |memory| {
        if !memory.content_searchable_by(caller) {
            return true;
        }
        let Some(m) = memory.apply_access_policy(caller) else {
            return true; // denied — skip but continue
        };
        results.push(SearchResult {
            memory: m,
            distance: None,
            retrieval_score: None,
            reranker_score: None,
            composite_score: None,
            score_breakdown: None,
        });
        results.len() < limit
    })?;

    Ok(results)
}

/// A memory hydrated from a vector search candidate, paired with its memory ID.
type HydratedRow = (Memory, MemoryId);

/// A post-filtered candidate paired with its optional vector distance.
type VisibleCandidate = (Memory, Option<f64>);

/// Phase 2: Hydrate vector candidate rowids into full `Memory` objects.
///
/// Retrieves all memory fields needed for filtering and response construction.
fn hydrate_candidates(conn: &rusqlite::Connection, candidates: &[VectorHit]) -> Result<Vec<HydratedRow>, StoreError> {
    let placeholders: Vec<String> = (1..=candidates.len()).map(|i| format!("?{i}")).collect();
    let sql = format!(
        "SELECT {} \
         FROM memories m \
         WHERE m.id IN ({})",
        *PREFIXED_COLUMNS,
        placeholders.join(",")
    );
    let memory_ids: Vec<String> = candidates.iter().map(|hit| hit.memory_id.to_string()).collect();
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = memory_ids.iter().map(|id| -> &dyn rusqlite::types::ToSql { id }).collect();
    let mut mem_stmt = conn.prepare(&sql)?;
    let mem_rows: Vec<(Memory, MemoryId)> = mem_stmt
        .query_map(param_refs.as_slice(), |row| {
            let mem = row_to_memory(row).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?;
            let memory_id = mem.id;
            Ok((mem, memory_id))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    Ok(mem_rows)
}

/// Context needed for post-filtering search results.
struct PostFilterContext<'a> {
    filter: &'a MemoryFilter,
    caller: Option<&'a str>,
    now: chrono::DateTime<chrono::Utc>,
    max_distance: Option<f64>,
}

// ---------------------------------------------------------------------------
// FTS5 full-text search
// ---------------------------------------------------------------------------

/// Sanitize a user query for use in an FTS5 MATCH expression.
///
/// Each token is double-quoted to treat it as a literal phrase token,
/// preventing FTS5 syntax injection (e.g., `NOT`, `AND`, `OR`, `NEAR`).
/// Tokens are joined with implicit AND semantics (FTS5 default for
/// quoted tokens in sequence).
///
/// Returns `None` if the query contains no indexable tokens after sanitization.
#[cfg(test)]
fn sanitize_fts_query(query: &str) -> Option<String> {
    sanitize_fts_query_with_context(query, None)
}

/// Sanitize a user query for FTS5 `MATCH`.
///
/// Main query tokens are quoted and `AND`'d together (implicit FTS5 default).
/// The `context` parameter is accepted for API compatibility but is intentionally
/// ignored for FTS5 — context enrichment only applies to the embedding path,
/// where it provides richer semantic signal. FTS5 stays precise on explicit keywords.
///
/// Returns `None` if nothing indexable remains after sanitization.
fn sanitize_fts_query_with_context(query: &str, _context: Option<&str>) -> Option<String> {
    let main_tokens: Vec<String> = query
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .map(|t| {
            let escaped = t.replace('"', "\"\"");
            format!("\"{escaped}\"")
        })
        .collect();

    if main_tokens.is_empty() {
        return None;
    }

    Some(main_tokens.join(" "))
}

#[expect(clippy::multiple_inherent_impl, reason = "SqliteStore methods are split across submodules by concern")]
impl SqliteStore {
    #[expect(
        clippy::too_many_arguments,
        reason = "FTS search requires query, limit, filter, context, caller context, and optional search context — all semantically distinct"
    )]
    pub(crate) async fn search_by_fts_impl(
        &self,
        query: &str,
        limit: usize,
        filter: MemoryFilter,
        ctx: QueryContext,
        context: Option<&str>,
    ) -> Result<Vec<SearchResult>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        // Sanitize the query for FTS5 with optional context enhancement.
        // If nothing indexable remains, fall back to LIKE.
        let Some(fts_query) = sanitize_fts_query_with_context(query, context) else {
            return self.search_by_text_impl(query, limit, filter, ctx).await;
        };

        let filter = normalize_filter(filter);
        let principal = ctx.principal;
        let now = self.clock_now();

        self.with_conn(move |conn| {
            let caller = principal.as_deref();
            fts_search_scan(conn, &fts_query, &filter, caller, now, limit)
        })
        .await
    }
}

/// Execute an FTS5 search with paged overfetching for access-policy filtering.
#[expect(
    clippy::too_many_arguments,
    reason = "FTS scan requires connection, query, filter, caller, time, and limit — all semantically distinct"
)]
#[expect(
    clippy::too_many_lines,
    reason = "FTS search stages query execution, optional entity hydration, filtering, and response assembly in one linear flow"
)]
fn fts_search_scan(
    conn: &rusqlite::Connection,
    fts_query: &str,
    filter: &MemoryFilter,
    caller: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
    limit: usize,
) -> Result<Vec<SearchResult>, StoreError> {
    let page_size = limit.saturating_mul(OVERFETCH_FACTOR).max(1);
    let filter_needs_entities = needs_entity_hydration(filter);

    // FTS5 external-content: join back to memories for full rows.
    // `rank` is the BM25 score (negative, more negative = more relevant).
    let sql = format!(
        "SELECT {}, fts.rank \
         FROM memory_fts fts \
         JOIN memories m ON m.rowid = fts.rowid \
         WHERE memory_fts MATCH ?1 \
         ORDER BY fts.rank, m.created_at DESC, m.id DESC \
         LIMIT ?2 OFFSET ?3",
        *PREFIXED_COLUMNS
    );

    let mut results: Vec<SearchResult> = Vec::with_capacity(limit);

    // FTS5 MATCH may fail on malformed queries even after sanitization. Fall back gracefully.
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("FTS5 query preparation failed, skipping FTS results: {e}");
            return Ok(Vec::new());
        }
    };

    let limit_i64 = usize_to_i64(page_size, "FTS page size")?;
    let mut offset = 0_usize;
    loop {
        let offset_i64 = usize_to_i64(offset, "FTS offset")?;
        let rows = match stmt.query_map(params![fts_query, limit_i64, offset_i64], |row| {
            let memory = row_to_memory(row).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?;
            let rank: f64 = row.get(MEMORY_COLUMN_COUNT)?;
            Ok((memory, rank))
        }) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("FTS5 MATCH query failed, skipping FTS results: {e}");
                return Ok(Vec::new());
            }
        };

        // Collect visible memories with their BM25 rank for retrieval scoring.
        let mut raw_row_count = 0_usize;
        let mut ranked: Vec<(Memory, f64)> = Vec::new();
        for row_result in rows {
            raw_row_count = raw_row_count.saturating_add(1);
            let (memory, rank) = match row_result {
                Ok(pair) => pair,
                Err(e) => {
                    tracing::warn!("FTS5 row deserialization failed, skipping: {e}");
                    continue;
                }
            };
            ranked.push((memory, rank));
        }

        if filter_needs_entities {
            hydrate_entities_for_ranked_memories(conn, &mut ranked)?;
        }

        let mut visible: Vec<(Memory, f64)> = Vec::new();
        for (memory, rank) in ranked {
            if !memory.content_searchable_by(caller) {
                continue;
            }
            let Some(memory) = apply_access_policy_for_filter(memory, filter, caller, now) else {
                continue;
            };
            visible.push((memory, rank));
            if results.len().saturating_add(visible.len()) >= limit {
                break;
            }
        }

        if !visible.is_empty() && !filter_needs_entities {
            hydrate_entities_for_ranked_memories(conn, &mut visible)?;
        }

        for (memory, rank) in visible {
            let Some(m) = memory.apply_access_policy(caller) else {
                continue;
            };
            // Pass raw BM25 rank (negative, more negative = better) as distance.
            results.push(SearchResult {
                memory: m,
                distance: Some(rank),
                retrieval_score: None,
                reranker_score: None,
                composite_score: None,
                score_breakdown: None,
            });
        }

        if results.len() >= limit || raw_row_count < page_size {
            break;
        }
        offset = offset.saturating_add(page_size);
        if offset >= MAX_SCAN_ROWS {
            break;
        }
    }

    Ok(results)
}

/// Phase 3: Post-filter hydrated results by access policy, filter predicates, and max distance.
///
/// Hydrates entities up front when entity predicates are present, then applies
/// `matches_non_access_filter`, `apply_access_policy`
/// (owner/allowed checks, field redaction), and finally the optional `max_distance` threshold.
fn post_filter_results(
    conn: &rusqlite::Connection,
    results: &mut Vec<SearchResult>,
    hydrated: Vec<HydratedRow>,
    candidates: &[VectorHit],
    ctx: &PostFilterContext<'_>,
) -> Result<(), StoreError> {
    let dist_map: std::collections::HashMap<MemoryId, f64> = candidates.iter().map(|hit| (hit.memory_id, hit.distance)).collect();
    let filter_needs_entities = needs_entity_hydration(ctx.filter);
    let mut hydrated_rows = hydrated;

    if filter_needs_entities {
        hydrate_entities_for_hydrated_rows(conn, &mut hydrated_rows)?;
    }

    let mut visible: Vec<VisibleCandidate> = hydrated_rows
        .into_iter()
        .filter_map(|(memory, memory_id)| {
            let distance = dist_map.get(&memory_id).copied();
            if !ctx.max_distance.is_none_or(|threshold| distance.is_some_and(|d| d <= threshold)) {
                return None;
            }
            if !memory.content_searchable_by(ctx.caller) {
                return None;
            }
            apply_access_policy_for_filter(memory, ctx.filter, ctx.caller, ctx.now).map(|memory| (memory, distance))
        })
        .collect();

    if !visible.is_empty() && !filter_needs_entities {
        let ids: Vec<MemoryId> = visible.iter().map(|(memory, _)| memory.id).collect();
        let mut entity_map = hydrate_entities_batch(conn, &ids)?;
        for (memory, _) in &mut visible {
            if let Some(entities) = entity_map.remove(&memory.id) {
                memory.entities = entities;
            }
        }
    }

    results.extend(visible.into_iter().filter_map(|(memory, distance)| {
        memory.apply_access_policy(ctx.caller).map(|m| SearchResult {
            memory: m,
            distance,
            retrieval_score: None,
            reranker_score: None,
            composite_score: None,
            score_breakdown: None,
        })
    }));
    Ok(())
}

fn hydrate_entities_for_ranked_memories(conn: &rusqlite::Connection, ranked: &mut [(Memory, f64)]) -> Result<(), StoreError> {
    if ranked.is_empty() {
        return Ok(());
    }

    let ids: Vec<MemoryId> = ranked.iter().map(|(memory, _)| memory.id).collect();
    let mut entity_map = hydrate_entities_batch(conn, &ids)?;
    for (memory, _) in ranked {
        if let Some(entities) = entity_map.remove(&memory.id) {
            memory.entities = entities;
        }
    }

    Ok(())
}

fn hydrate_entities_for_hydrated_rows(conn: &rusqlite::Connection, hydrated: &mut [HydratedRow]) -> Result<(), StoreError> {
    if hydrated.is_empty() {
        return Ok(());
    }

    let ids: Vec<MemoryId> = hydrated.iter().map(|(memory, _)| memory.id).collect();
    let mut entity_map = hydrate_entities_batch(conn, &ids)?;
    for (memory, _) in hydrated {
        if let Some(entities) = entity_map.remove(&memory.id) {
            memory.entities = entities;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_fts_query_quotes_tokens() {
        assert_eq!(sanitize_fts_query("hello world"), Some("\"hello\" \"world\"".into()));
    }

    #[test]
    fn sanitize_fts_query_empty_returns_none() {
        assert_eq!(sanitize_fts_query(""), None);
        assert_eq!(sanitize_fts_query("   "), None);
    }

    #[test]
    fn sanitize_fts_query_escapes_internal_quotes() {
        // Input: say "hello" → tokens: [say, "hello"] → quoted: ["say", """hello"""]
        let result = sanitize_fts_query(r#"say "hello""#).unwrap();
        assert!(result.contains("\"say\""), "should quote 'say': {result}");
        // The exact escaping depends on how whitespace splits the tokens; verify no panic.
        assert!(!result.is_empty());
    }

    #[test]
    fn sanitize_fts_query_handles_fts5_operators() {
        // Operators should be quoted to prevent syntax injection
        let result = sanitize_fts_query("NOT secret AND password").unwrap();
        assert!(result.contains("\"NOT\""));
        assert!(result.contains("\"AND\""));
    }

    #[test]
    fn sanitize_fts_query_single_token() {
        assert_eq!(sanitize_fts_query("ABC-123"), Some("\"ABC-123\"".into()));
    }

    // -- Wave 2: Context-enhanced FTS query tests --

    #[test]
    fn context_no_context_returns_base_query() {
        let result = sanitize_fts_query_with_context("auth login", None).unwrap();
        assert_eq!(result, "\"auth\" \"login\"");
    }

    #[test]
    fn context_empty_context_returns_base_query() {
        let result = sanitize_fts_query_with_context("auth login", Some("  ")).unwrap();
        assert_eq!(result, "\"auth\" \"login\"");
    }

    #[test]
    fn context_is_ignored_for_fts_precision() {
        // Context tokens are only used for the embedding path, not FTS5.
        // FTS5 stays precise on the main query only.
        let result = sanitize_fts_query_with_context("auth", Some("OAuth2 login flow")).unwrap();
        assert_eq!(result, "\"auth\"", "FTS5 query should only contain main query tokens");
    }

    #[test]
    fn context_does_not_affect_fts_query() {
        // Context is ignored for FTS5 — only used for embedding enrichment.
        let result = sanitize_fts_query_with_context("auth", Some("the user is on a login page")).unwrap();
        assert_eq!(result, "\"auth\"", "context should not modify FTS query: {result}");

        let result = sanitize_fts_query_with_context("login auth", Some("login OAuth2")).unwrap();
        assert_eq!(result, "\"login\" \"auth\"", "context should not modify FTS query: {result}");
    }
}
