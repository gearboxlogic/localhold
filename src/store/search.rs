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

use zerocopy::IntoBytes as _;

use super::{
    SqliteStore,
    crud::hydrate_entities_batch,
    query::{
        MAX_SCAN_ROWS, MAX_VEC_CANDIDATES, MEMORY_COLUMN_COUNT, OVERFETCH_FACTOR, ScanConfig, WhereClause, apply_access_policy_for_filter, escape_like, needs_entity_hydration,
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
            super::vector::validate_embedding_vector(&emb, vector_index.dimensions())?;
            let caller = principal.as_deref();
            let pf_ctx = PostFilterContext {
                filter: &filter,
                caller,
                now,
                max_distance,
            };
            if filter.context_ids.is_some() || filter.legacy_context_ids_any.as_ref().is_some_and(|context_ids| !context_ids.is_empty()) {
                filtered_embedding_search_loop(conn, &emb, limit, &pf_ctx)
            } else {
                embedding_search_loop(conn, &vector_index, &emb, limit, &pf_ctx)
            }
        })
        .await
    }

    /// Find nearest neighbors within the canonical consolidation candidate set.
    ///
    /// Context and authorization filtering happens while constructing
    /// `candidate_ids`; constraining the SQL before ordering and limiting keeps
    /// unrelated vectors from consuming the bounded neighbor budget.
    #[expect(
        clippy::too_many_arguments,
        reason = "candidate-bounded neighbor lookup needs source, candidates, vector, threshold, and limit"
    )]
    pub(crate) async fn find_embedding_neighbors_impl(
        &self,
        source_memory_id: &MemoryId,
        candidate_ids: &[MemoryId],
        embedding: &[f32],
        min_cosine_similarity: f64,
        limit: usize,
    ) -> Result<Vec<super::EmbeddingNeighbor>, StoreError> {
        if limit == 0 || candidate_ids.is_empty() {
            return Ok(Vec::new());
        }
        let emb = embedding.to_vec();
        let source_memory_id = source_memory_id.to_string();
        let candidate_ids = serde_json::to_string(&candidate_ids.iter().map(ToString::to_string).collect::<Vec<_>>())?;
        let vector_index = self.vector_index();
        self.with_conn(move |conn| {
            super::vector::validate_embedding_vector(&emb, vector_index.dimensions())?;
            let emb_bytes: &[u8] = emb.as_bytes();
            let mut statement = conn.prepare(
                "WITH ranked_candidates AS MATERIALIZED (
                    SELECT embedding_map.memory_id,
                           1.0 - vec_distance_cosine(vector_row.embedding, ?1) AS similarity
                    FROM memory_embeddings AS vector_row
                    JOIN memory_embedding_map AS embedding_map
                      ON embedding_map.vec_rowid = vector_row.rowid
                    JOIN memories AS memory_row
                      ON memory_row.id = embedding_map.memory_id
                    WHERE embedding_map.memory_id IN (
                              SELECT CAST(value AS TEXT) FROM json_each(?2)
                          )
                      AND embedding_map.memory_id <> ?3
                      AND memory_row.superseded_by IS NULL
                 )
                 SELECT memory_id, similarity
                 FROM ranked_candidates
                 WHERE similarity >= ?4
                   AND similarity <= 1.0
                 ORDER BY similarity DESC, memory_id
                 LIMIT ?5",
            )?;
            let rows = statement
                .query_map(
                    rusqlite::params![emb_bytes, candidate_ids, source_memory_id, min_cosine_similarity, usize_to_i64(limit, "neighbor limit")?,],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)),
                )?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows
                .into_iter()
                .filter_map(|(id, similarity)| id.parse().ok().map(|memory_id| (memory_id, similarity)))
                .collect())
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

/// Exact vector ranking over a bounded SQL-prefiltered governed candidate set.
///
/// sqlite-vec's KNN virtual-table query cannot express normalized
/// many-to-many context applicability. The indexed membership predicate first
/// selects a stable, recency-ordered candidate set under the hard ceiling;
/// `vec_distance_L2` then ranks only that bounded set inside SQLite.
fn filtered_embedding_search_loop(conn: &rusqlite::Connection, emb: &[f32], limit: usize, pf_ctx: &PostFilterContext<'_>) -> Result<Vec<SearchResult>, StoreError> {
    let wc = WhereClause::from_filter(pf_ctx.filter, pf_ctx.caller, 2, pf_ctx.now);
    let where_sql = wc.to_where_sql();
    let candidate_limit_index = wc.next_index();
    let cursor_distance_index = candidate_limit_index.saturating_add(1);
    let cursor_id_index = cursor_distance_index.saturating_add(1);
    let limit_index = cursor_id_index.saturating_add(1);
    let sql = filtered_embedding_search_sql(&where_sql, candidate_limit_index, cursor_distance_index, cursor_id_index, limit_index);
    let emb_bytes: &[u8] = emb.as_bytes();
    let page_size = limit.saturating_mul(OVERFETCH_FACTOR).max(1);
    let mut scanned_rows = 0_usize;
    let mut cursor_distance = None::<f64>;
    let mut cursor_id = String::new();
    let mut results = Vec::with_capacity(limit);
    let candidate_limit = usize_to_i64(MAX_VEC_CANDIDATES, "governed vector candidate limit")?;
    loop {
        let request_size = page_size.min(MAX_VEC_CANDIDATES.saturating_sub(scanned_rows));
        if request_size == 0 {
            tracing::info!(
                max = MAX_VEC_CANDIDATES,
                collected = results.len(),
                requested = limit,
                "governed vector search exiting: reached MAX_VEC_CANDIDATES ceiling"
            );
            break;
        }
        let page_limit = usize_to_i64(request_size, "governed vector page size")?;
        let mut bindings: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(wc.params().len().saturating_add(4));
        bindings.push(&emb_bytes);
        for value in wc.params() {
            bindings.push(value);
        }
        bindings.push(&candidate_limit);
        bindings.push(&cursor_distance);
        bindings.push(&cursor_id);
        bindings.push(&page_limit);
        let mut statement = conn.prepare(&sql)?;
        let rows = statement
            .query_map(&*bindings, |row| Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let row_count = rows.len();
        if let Some((last_id, last_distance)) = rows.last() {
            cursor_id.clone_from(last_id);
            cursor_distance = Some(*last_distance);
        }
        let hits = rows
            .into_iter()
            .filter_map(|(id, distance)| id.parse().ok().map(|memory_id| VectorHit { memory_id, distance }))
            .collect::<Vec<_>>();
        if !hits.is_empty() {
            let hydrated = hydrate_candidates(conn, &hits)?;
            post_filter_results(conn, &mut results, hydrated, &hits, pf_ctx)?;
        }
        if results.len() >= limit || row_count < request_size {
            break;
        }
        scanned_rows = scanned_rows.saturating_add(row_count);
        if scanned_rows >= MAX_VEC_CANDIDATES {
            tracing::info!(
                max = MAX_VEC_CANDIDATES,
                collected = results.len(),
                requested = limit,
                "governed vector search exiting: reached MAX_VEC_CANDIDATES ceiling"
            );
            break;
        }
    }
    sort_by_distance(&mut results);
    results.truncate(limit);
    Ok(results)
}

fn filtered_embedding_search_sql(where_sql: &str, candidate_limit_index: usize, cursor_distance_index: usize, cursor_id_index: usize, limit_index: usize) -> String {
    format!(
        "WITH candidate_ids AS MATERIALIZED (
             SELECT memories.id AS memory_id
             FROM memories{where_sql}
             ORDER BY memories.created_at DESC, memories.id DESC
             LIMIT ?{candidate_limit_index}
         ),
         ranked_candidates AS MATERIALIZED (
             SELECT embedding_map.memory_id,
                    vec_distance_L2(vector_row.embedding, ?1) AS distance
             FROM memory_embeddings AS vector_row
             JOIN memory_embedding_map AS embedding_map
               ON embedding_map.vec_rowid = vector_row.rowid
             JOIN candidate_ids
               ON candidate_ids.memory_id = embedding_map.memory_id
         )
         SELECT memory_id, distance
         FROM ranked_candidates
         WHERE (
             ?{cursor_distance_index} IS NULL
             OR distance > ?{cursor_distance_index}
             OR (distance = ?{cursor_distance_index} AND memory_id > ?{cursor_id_index})
         )
         ORDER BY distance, memory_id
         LIMIT ?{limit_index}"
    )
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

    // FTS5 external-content: join back to memories for full rows. The
    // membership subquery applies every filter before BM25 candidate ranking.
    let wc = WhereClause::from_filter(filter, caller, 2, now);
    let where_sql = wc.to_where_sql();
    let limit_index = wc.next_index();
    let offset_index = limit_index.saturating_add(1);
    let sql = format!(
        "SELECT {}, fts.rank \
         FROM memory_fts fts \
         JOIN memories m ON m.rowid = fts.rowid \
         WHERE memory_fts MATCH ?1
           AND m.id IN (SELECT memories.id FROM memories{where_sql}) \
         ORDER BY fts.rank, m.created_at DESC, m.id DESC \
         LIMIT ?{limit_index} OFFSET ?{offset_index}",
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
        let fts_params = vec![fts_query.to_owned()];
        let bindings = wc.bind_params(&fts_params, &limit_i64, &offset_i64);
        let rows = match stmt.query_map(&*bindings, |row| {
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
    fn governed_vector_sql_caps_candidates_before_distance_evaluation() {
        let sql = filtered_embedding_search_sql(" WHERE governed = ?2", 3, 4, 5, 6);
        let candidate_stage = sql.split("ranked_candidates").next().unwrap();
        let distance_position = sql.find("vec_distance_L2").unwrap();
        let candidate_limit_position = sql.find("LIMIT ?3").unwrap();

        assert!(candidate_stage.contains("WHERE governed = ?2"));
        assert!(candidate_limit_position < distance_position, "candidate LIMIT must precede vector distance evaluation");
        assert!(sql.contains("JOIN candidate_ids"), "distance evaluation must read only the bounded candidate CTE");
    }

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
