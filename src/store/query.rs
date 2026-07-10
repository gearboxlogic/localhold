//! Query building, paging helpers, and row deserialization for the memories table.

use rusqlite::params;

use super::crud::hydrate_entities_batch;
use crate::{
    error::StoreError,
    ordering,
    types::{AccessLevel, AccessPolicy, Memory, MemoryFilter, MemoryId, MemoryStats, MemoryType, Provenance, SearchResult},
};

/// Concatenate string literals with `", "` separators at compile time.
macro_rules! concat_with_sep {
    ($first:literal $(, $rest:literal)*) => {
        concat!($first $(, ", ", $rest)*)
    };
}

/// Define the canonical column list for the `memories` table in a single place.
///
/// Expands to `COLUMNS`, `MEMORY_COLUMN_COUNT`, and `MEMORY_COLUMNS` — keeping
/// every column reference in sync automatically.
///
/// **Ordering contract**: new columns are appended at the end.
/// Update `row_to_memory` when adding columns.
macro_rules! define_memory_columns {
    ($($col:literal),+ $(,)?) => {
        /// Canonical ordered column names for the `memories` table.
        pub(crate) const COLUMNS: &[&str] = &[$($col),+];

        /// Number of columns in [`COLUMNS`].
        ///
        /// Use this to index extra columns appended after the base set (e.g.
        /// `vec_rowid`, `rank`) instead of hard-coding magic numbers.
        pub(crate) const MEMORY_COLUMN_COUNT: usize = COLUMNS.len();

        /// Comma-separated column list for `SELECT` queries, built from [`COLUMNS`].
        pub(crate) const MEMORY_COLUMNS: &str = concat_with_sep!($($col),+);
    };
}

define_memory_columns![
    "id",                // 0
    "content",           // 1
    "tags",              // 2
    "provenance",        // 3
    "access_policy",     // 4
    "created_at",        // 5
    "expires_at",        // 6
    "has_embedding",     // 7
    "memory_type",       // 8
    "importance",        // 9
    "impression_count",  // 10
    "last_impressed_at", // 11
    "superseded_by",     // 12
    "activity_mass",     // 13
    "last_used_at",      // 14
    "updated_at",        // 15
    "confidence",        // 16
];

/// Overfetch multiplier for SQL LIMIT — accounts for rows filtered in Rust by access policy.
pub(crate) const OVERFETCH_FACTOR: usize = 4;

/// Hard ceiling on vector search candidates to prevent unbounded queries.
pub(crate) const MAX_VEC_CANDIDATES: usize = crate::config::MAX_CANDIDATE_POOL_SIZE_CEILING;

/// Hard ceiling on total rows scanned in paging loops to prevent unbounded scans.
pub(crate) const MAX_SCAN_ROWS: usize = 10_000;

/// Default maximum number of memories returned by `list()` when no explicit limit is provided.
pub(crate) const DEFAULT_LIST_LIMIT: usize = 100;

/// Page size for the counting scan in `count_with_access_filter`.
const COUNT_PAGE_SIZE: usize = 500;

/// Convert a `usize` to `i64` for SQL parameter binding, returning a
/// descriptive [`StoreError::Database`] on overflow.
pub(crate) fn usize_to_i64(value: usize, context: &str) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|e| StoreError::Database(format!("{context} {value} exceeds i64 range: {e}").into()))
}

fn sqlite_u64(row: &rusqlite::Row<'_>) -> rusqlite::Result<u64> {
    let value: i64 = row.get(0)?;
    u64::try_from(value).map_err(|_err| rusqlite::Error::IntegralValueOutOfRange(0, value))
}

/// Escape LIKE special characters for safe use in SQL LIKE patterns.
///
/// Single-pass: scans the input once, appending escape prefixes as needed,
/// instead of the three separate `replace()` calls that would each allocate
/// and scan the full string.
pub(crate) fn escape_like(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' | '%' | '_' => {
                result.push('\\');
                result.push(c);
            }
            _ => result.push(c),
        }
    }
    result
}

/// Allocation-free case-insensitive ASCII substring search.
///
/// The `needle` must already be lowercased (e.g. via `normalize_filter`).
/// Compares by lowering each byte of `haystack` on the fly, avoiding a
/// full `to_lowercase()` allocation. Falls back to `str::to_lowercase()`
/// for non-ASCII content where byte-level lowering is incorrect.
fn contains_case_insensitive_ascii(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    // Fast path: ASCII-only content can use byte-level case folding.
    if haystack.is_ascii() {
        let needle_bytes = needle.as_bytes();
        let haystack_bytes = haystack.as_bytes();
        if needle_bytes.len() > haystack_bytes.len() {
            return false;
        }
        let end = haystack_bytes.len().saturating_add(1).saturating_sub(needle_bytes.len());
        for start in 0..end {
            if haystack_bytes[start..start.saturating_add(needle_bytes.len())]
                .iter()
                .zip(needle_bytes.iter())
                .all(|(h, n)| h.to_ascii_lowercase() == *n)
            {
                return true;
            }
        }
        false
    } else {
        // Slow path: non-ASCII content requires proper Unicode lowering.
        haystack.to_lowercase().contains(needle)
    }
}

/// Pre-lowercase the `text_search` field for the Rust-side content filter used in
/// the overfetch/paging loop (`matches_non_access_filter`).  The SQL `LIKE` clause
/// handles its own case-insensitivity, but rows that survive the SQL layer are
/// re-checked in Rust (for access policy, TTL, etc.) where we need a case-insensitive
/// text match — lowercasing once here avoids per-row `.to_lowercase()` on the query.
pub(crate) fn normalize_filter(mut filter: MemoryFilter) -> MemoryFilter {
    if let Some(ref mut text) = filter.text_search {
        let trimmed = text.trim().to_lowercase();
        if trimmed.is_empty() {
            filter.text_search = None;
        } else {
            *text = trimmed;
        }
    }
    filter
}

/// Parse a row from the memories table into a Memory struct.
///
/// Column indices follow [`COLUMNS`]: 0=`id`, 1=`content`, 2=`tags`, 3=`provenance`,
/// 4=`access_policy`, 5=`created_at`, 6=`expires_at`, 7=`has_embedding`, 8=`memory_type`,
/// 9=`importance`, 10=`impression_count`, 11=`last_impressed_at`, 12=`superseded_by`,
/// 13=`activity_mass`, 14=`last_used_at`, 15=`updated_at`, 16=`confidence`.
#[expect(
    clippy::too_many_lines,
    reason = "deserialization of 17 columns is inherently sequential; splitting would obscure the column mapping"
)]
pub(crate) fn row_to_memory(row: &rusqlite::Row<'_>) -> Result<Memory, StoreError> {
    let id_str: String = row.get(0)?;
    let content: String = row.get(1)?;
    let tags_json: String = row.get(2)?;
    let provenance_json: String = row.get(3)?;
    let access_json: String = row.get(4)?;
    let created_at_str: String = row.get(5)?;
    let expires_at_str: Option<String> = row.get(6)?;
    let has_embedding: bool = row.get(7)?;
    let memory_type_str: String = row.get(8)?;
    let importance_raw: f64 = row.get(9)?;
    let importance = crate::types::Importance::new(importance_raw);
    let impression_count_i64: i64 = row.get(10)?;
    let last_impressed_at_str: Option<String> = row.get(11)?;
    let superseded_by_str: Option<String> = row.get(12)?;
    let activity_mass: f64 = row.get(13)?;
    let last_used_at_str: Option<String> = row.get(14)?;
    let updated_at_str: Option<String> = row.get(15)?;
    let confidence_val: f64 = row.get(16)?;

    let id: MemoryId = id_str.parse().map_err(|e| StoreError::Serialization(format!("invalid memory id: {e}").into()))?;
    let superseded_by: Option<MemoryId> = superseded_by_str
        .map(|s| s.parse::<MemoryId>())
        .transpose()
        .map_err(|e| StoreError::Serialization(format!("invalid superseded_by id: {e}").into()))?;
    let tags: Vec<String> = serde_json::from_str(&tags_json)?;
    let provenance: Provenance = serde_json::from_str(&provenance_json)?;
    let access_policy: AccessPolicy = serde_json::from_str(&access_json)?;
    let created_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
        .map_err(|e| StoreError::Serialization(format!("invalid datetime: {e}").into()))?
        .with_timezone(&chrono::Utc);
    let expires_at = expires_at_str
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| StoreError::Serialization(format!("invalid expires_at: {e}").into()))
        })
        .transpose()?;
    let memory_type: MemoryType = memory_type_str
        .parse()
        .map_err(|e: crate::error::ParseEnumError| StoreError::Serialization(e.to_string().into()))?;
    #[expect(clippy::cast_sign_loss, reason = "i64 → u64 cast: impression_count is always non-negative by DB constraint")]
    #[expect(clippy::as_conversions, reason = "i64 → u64 cast: impression_count is always non-negative by DB constraint")]
    let impression_count = impression_count_i64 as u64;
    let last_impressed_at = last_impressed_at_str
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| StoreError::Serialization(format!("invalid last_impressed_at: {e}").into()))
        })
        .transpose()?;
    let last_used_at = last_used_at_str
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| StoreError::Serialization(format!("invalid last_used_at: {e}").into()))
        })
        .transpose()?;
    // Fallback to created_at for rows that predate the migration backfill.
    let updated_at = updated_at_str
        .map(|s| {
            chrono::DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&chrono::Utc))
                .map_err(|e| StoreError::Serialization(format!("invalid updated_at: {e}").into()))
        })
        .transpose()?
        .unwrap_or(created_at);

    Ok(Memory {
        id,
        content,
        tags,
        provenance,
        access_policy,
        created_at,
        updated_at,
        expires_at,
        has_embedding,
        memory_type,
        importance,
        confidence: crate::types::Confidence::new(confidence_val),
        impression_count,
        last_impressed_at,
        superseded_by,
        activity_mass,
        last_used_at,
        // Entities are stored in a junction table and hydrated separately.
        entities: Vec::new(),
        was_redacted: false,
    })
}

/// Sort search results by distance with a deterministic ID tiebreaker.
pub(crate) fn sort_by_distance(results: &mut [SearchResult]) {
    results.sort_by(ordering::cmp_search_result_distance_asc);
}

/// Returns `true` when a filter needs entity hydration before Rust-side filtering.
pub(crate) const fn needs_entity_hydration(filter: &MemoryFilter) -> bool {
    filter.entity.is_some() || filter.entities_any.is_some() || filter.entity_type.is_some()
}

/// Check if a memory matches the non-access filter criteria (tags, provenance,
/// entities, `text_search`, TTL). Access policy is handled separately via
/// `apply_access_policy`.
///
/// Entity filters require `memory.entities` to be hydrated before calling this
/// function.
#[expect(clippy::too_many_lines, reason = "each filter field adds a short guard clause — splitting would hurt readability")]
pub(crate) fn matches_non_access_filter(memory: &Memory, filter: &MemoryFilter, now: chrono::DateTime<chrono::Utc>) -> bool {
    if let Some(tags) = &filter.tags
        && !tags.is_empty()
        && !tags.iter().any(|t| memory.tags.contains(t))
    {
        return false;
    }
    if let Some(agent) = &filter.agent_label
        && memory.provenance.source_agent.as_ref() != Some(agent)
    {
        return false;
    }
    if let Some(scope) = &filter.scope
        && memory.provenance.source_conversation.as_ref() != Some(scope)
    {
        return false;
    }
    if let Some(origin_scope) = &filter.origin_scope
        && memory.provenance.origin_conversation.as_ref().or(memory.provenance.source_conversation.as_ref()) != Some(origin_scope)
    {
        return false;
    }
    if let Some(scopes_any) = &filter.scopes_any
        && !scopes_any.is_empty()
        && memory.provenance.source_conversation.as_ref().is_none_or(|scope_key| !scopes_any.contains(scope_key))
    {
        return false;
    }
    if let Some(entity) = &filter.entity
        && !memory.entities.iter().any(|candidate| candidate.name == *entity)
    {
        return false;
    }
    if let Some(entities) = &filter.entities_any
        && !entities.is_empty()
        && !memory.entities.iter().any(|candidate| entities.contains(&candidate.name))
    {
        return false;
    }
    if let Some(entity_type) = &filter.entity_type
        && !memory.entities.iter().any(|candidate| candidate.entity_type.as_ref() == entity_type.as_str())
    {
        return false;
    }
    if let Some(range) = &filter.time_range {
        if let Some(after) = range.after
            && memory.created_at < after
        {
            return false;
        }
        if let Some(before) = range.before
            && memory.created_at >= before
        {
            return false;
        }
    }
    // `text` is already lowercased by `normalize_filter`; use allocation-free
    // byte-by-byte comparison instead of `content.to_lowercase()`.
    if let Some(text) = &filter.text_search
        && !contains_case_insensitive_ascii(&memory.content, text)
    {
        return false;
    }
    // has_embedding filter
    if let Some(has_emb) = filter.has_embedding
        && memory.has_embedding != has_emb
    {
        return false;
    }
    // memory_type filter
    if let Some(mt) = filter.memory_type
        && memory.memory_type != mt
    {
        return false;
    }
    // Supersession filter — hide superseded memories unless explicitly included.
    if !filter.include_superseded.unwrap_or(false) && memory.superseded_by.is_some() {
        return false;
    }
    // Lazy TTL enforcement — expired memories are invisible.
    if memory.expires_at.is_some_and(|exp| now >= exp) {
        return false;
    }
    true
}

fn matches_lifecycle_filter(memory: &Memory, filter: &MemoryFilter, now: chrono::DateTime<chrono::Utc>) -> bool {
    if !filter.include_superseded.unwrap_or(false) && memory.superseded_by.is_some() {
        return false;
    }
    memory.expires_at.is_none_or(|exp| now < exp)
}

/// Apply access policy before returning a memory from a filtered query.
///
/// SQL predicates may use raw columns for performance, but redacted callers must
/// not be able to confirm hidden tags, scope/provenance, entities, or content by
/// adding filters. Re-checking the filter against the caller-visible view keeps
/// hidden fields from becoming an oracle while preserving raw lifecycle checks.
#[must_use]
pub(crate) fn apply_access_policy_for_filter(memory: Memory, filter: &MemoryFilter, caller: Option<&str>, now: chrono::DateTime<chrono::Utc>) -> Option<Memory> {
    if !matches_lifecycle_filter(&memory, filter, now) {
        return None;
    }
    let visible = match memory.check_access_level(caller) {
        AccessLevel::Full => memory,
        AccessLevel::Redacted => memory.redacted(),
        AccessLevel::Denied => return None,
    };
    if !matches_non_access_filter(&visible, filter, now) {
        return None;
    }
    if filter.text_search.is_some() && !visible.content_searchable_by(caller) {
        return None;
    }
    Some(visible)
}

/// Result of consuming a `WhereClause` with paging — carries the parameter indices for LIMIT/OFFSET.
/// Dynamic WHERE clause builder for pushing `MemoryFilter` conditions into SQL.
///
/// All parameter values are stored as `String` so they can be re-borrowed across
/// paging iterations without cloning boxed trait objects.
struct WhereClause {
    conditions: Vec<String>,
    params: Vec<String>,
    next_idx: usize,
}

impl WhereClause {
    /// Build SQL conditions from a `MemoryFilter`, starting parameter numbering at `start`.
    ///
    /// `caller` is the requesting principal: when `None`, an additional condition restricts
    /// results to public-only memories.
    #[expect(clippy::arithmetic_side_effects, reason = "SQL parameter index arithmetic — param count is always small")]
    #[expect(clippy::too_many_lines, reason = "linear filter-to-SQL translation reads clearly top-to-bottom")]
    fn from_filter(filter: &MemoryFilter, caller: Option<&str>, start: usize, now: chrono::DateTime<chrono::Utc>) -> Self {
        let mut wc = Self {
            conditions: Vec::new(),
            params: Vec::new(),
            next_idx: start,
        };

        // TTL: exclude expired memories
        let now = now.to_rfc3339();
        wc.push_str(format!("(expires_at IS NULL OR expires_at > ?{})", wc.next_idx), now);

        // time_range bounds
        if let Some(range) = &filter.time_range {
            if let Some(after) = range.after {
                let idx = wc.next_idx;
                wc.push_str(format!("created_at >= ?{idx}"), after.to_rfc3339());
            }
            if let Some(before) = range.before {
                let idx = wc.next_idx;
                wc.push_str(format!("created_at < ?{idx}"), before.to_rfc3339());
            }
        }

        // agent_label filter (uses expression index on provenance.source_agent)
        if let Some(agent) = &filter.agent_label {
            let idx = wc.next_idx;
            wc.push_str(format!("json_extract(provenance, '$.source_agent') = ?{idx}"), agent.clone());
        }

        // scope filter (uses expression index on provenance.source_conversation)
        if let Some(scope) = &filter.scope {
            let idx = wc.next_idx;
            wc.push_str(format!("json_extract(provenance, '$.source_conversation') = ?{idx}"), scope.clone());
        }

        // origin_scope filter (uses expression index on provenance.origin_conversation)
        if let Some(origin_scope) = &filter.origin_scope {
            let idx = wc.next_idx;
            wc.push_str(
                format!("COALESCE(json_extract(provenance, '$.origin_conversation'), json_extract(provenance, '$.source_conversation')) = ?{idx}"),
                origin_scope.clone(),
            );
        }

        // scopes_any (any-match against provenance.source_conversation)
        if let Some(scopes_any) = &filter.scopes_any
            && !scopes_any.is_empty()
        {
            let placeholders: Vec<String> = (0..scopes_any.len()).map(|i| format!("?{}", wc.next_idx + i)).collect();
            wc.conditions
                .push(format!("json_extract(provenance, '$.source_conversation') IN ({})", placeholders.join(", ")));
            for scope_key in scopes_any {
                wc.params.push(scope_key.clone());
            }
            wc.next_idx += scopes_any.len();
        }

        // text_search (LIKE with escaped wildcards)
        if let Some(text) = &filter.text_search {
            let pattern = format!("%{}%", escape_like(text));
            let idx = wc.next_idx;
            wc.push_str(format!("content LIKE ?{idx} ESCAPE '\\'"), pattern);
        }

        // has_embedding filter
        if let Some(has_emb) = filter.has_embedding {
            let idx = wc.next_idx;
            wc.push_str(format!("has_embedding = ?{idx}"), if has_emb { "1" } else { "0" }.to_owned());
        }

        // tags (any-match via json_each correlated subquery)
        if let Some(tags) = &filter.tags
            && !tags.is_empty()
        {
            let placeholders: Vec<String> = (0..tags.len()).map(|i| format!("?{}", wc.next_idx + i)).collect();
            wc.conditions
                .push(format!("EXISTS (SELECT 1 FROM json_each(tags) AS t WHERE t.value IN ({}))", placeholders.join(", ")));
            for tag in tags {
                wc.params.push(tag.clone());
            }
            wc.next_idx += tags.len();
        }

        // memory_type filter (uses index on memory_type)
        if let Some(mt) = &filter.memory_type {
            let idx = wc.next_idx;
            wc.push_str(format!("memory_type = ?{idx}"), mt.to_string());
        }

        // Supersession filter — exclude superseded memories by default
        if !filter.include_superseded.unwrap_or(false) {
            wc.conditions.push("superseded_by IS NULL".into());
        }

        // Entity name filter (EXISTS subquery on memory_entities junction table)
        if let Some(entity) = &filter.entity {
            let idx = wc.next_idx;
            wc.push_str(
                format!("EXISTS (SELECT 1 FROM memory_entities me WHERE me.memory_id = memories.id AND me.entity = ?{idx})"),
                entity.clone(),
            );
        }

        // Entity any-match filter (EXISTS subquery with IN list on memory_entities)
        if let Some(entities) = &filter.entities_any
            && !entities.is_empty()
        {
            let placeholders: Vec<String> = (0..entities.len()).map(|i| format!("?{}", wc.next_idx + i)).collect();
            wc.conditions.push(format!(
                "EXISTS (SELECT 1 FROM memory_entities me WHERE me.memory_id = memories.id AND me.entity IN ({}))",
                placeholders.join(", ")
            ));
            for entity in entities {
                wc.params.push(entity.clone());
            }
            wc.next_idx += entities.len();
        }

        // Entity type filter (EXISTS subquery on memory_entities junction table)
        if let Some(entity_type) = &filter.entity_type {
            let idx = wc.next_idx;
            wc.push_str(
                format!("EXISTS (SELECT 1 FROM memory_entities me WHERE me.memory_id = memories.id AND me.entity_type = ?{idx})"),
                entity_type.clone(),
            );
        }

        // When no principal is available, only public memories are visible (uses expression index)
        if caller.is_none() {
            wc.conditions.push("json_extract(access_policy, '$.type') = 'public'".into());
        }

        wc
    }

    #[expect(clippy::arithmetic_side_effects, reason = "SQL parameter index increment — param count is always small")]
    fn push_str(&mut self, condition: String, value: String) {
        self.conditions.push(condition);
        self.params.push(value);
        self.next_idx += 1;
    }

    /// Produce the ` WHERE ...` SQL fragment (including leading space), or empty string if no conditions.
    fn to_where_sql(&self) -> String {
        if self.conditions.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", self.conditions.join(" AND "))
        }
    }

    /// Produce the WHERE SQL fragment with optional extra conditions prepended.
    fn to_where_sql_with_extra(&self, extra_where: Option<&str>) -> String {
        extra_where.map_or_else(
            || self.to_where_sql(),
            |extra| {
                let mut conditions = vec![extra.to_owned()];
                conditions.extend(self.conditions.iter().cloned());
                format!(" WHERE {}", conditions.join(" AND "))
            },
        )
    }

    /// The parameter index that would be assigned to the next appended parameter.
    const fn next_index(&self) -> usize {
        self.next_idx
    }

    /// Build a `&dyn ToSql` reference slice for binding, combining extra params, filter params,
    /// and paging values (limit/offset).
    fn bind_params<'a>(&'a self, extra_params: &'a [String], limit: &'a i64, offset: &'a i64) -> Vec<&'a dyn rusqlite::types::ToSql> {
        let mut refs: Vec<&dyn rusqlite::types::ToSql> = Vec::with_capacity(extra_params.len().saturating_add(self.params.len()).saturating_add(2));
        for ep in extra_params {
            refs.push(ep);
        }
        for p in &self.params {
            refs.push(p);
        }
        refs.push(limit);
        refs.push(offset);
        refs
    }
}

/// Accumulation callback for paged scans — returns `true` to continue, `false` to stop.
///
/// Receives owned `Memory` values so callers avoid cloning when they need ownership.
type Accumulator<'a> = &'a mut dyn FnMut(Memory) -> bool;

/// Generic paged scan configuration for memory queries.
///
/// Encapsulates the common parameters for the paged scan loop used by `list`,
/// `search_by_text`, and `count`. This eliminates ~80 lines of structural
/// duplication that previously existed across these three query paths.
pub(crate) struct ScanConfig<'a> {
    /// Database connection.
    conn: &'a rusqlite::Connection,
    /// Filter predicates for the query.
    filter: &'a MemoryFilter,
    /// Caller agent for access policy evaluation.
    caller: Option<&'a str>,
    /// Current time for TTL enforcement.
    now: chrono::DateTime<chrono::Utc>,
    /// Number of rows per page.
    page_size: usize,
}

impl<'a> ScanConfig<'a> {
    /// Create a new scan configuration.
    pub(crate) const fn new(conn: &'a rusqlite::Connection, filter: &'a MemoryFilter, caller: Option<&'a str>, now: chrono::DateTime<chrono::Utc>, page_size: usize) -> Self {
        Self {
            conn,
            filter,
            caller,
            now,
            page_size,
        }
    }

    /// Execute the paged scan, calling `accumulate` for each post-filtered row.
    ///
    /// Returns `true` from the closure to continue scanning, `false` to stop.
    pub(crate) fn run(self, mut accumulate: impl FnMut(Memory) -> bool) -> Result<(), StoreError> {
        paged_scan_loop(&self, None, &[], false, &mut accumulate)
    }

    /// Execute the paged scan, hydrating entities before calling `accumulate`.
    ///
    /// Used by list/search flows that rely on `apply_access_policy` to decide
    /// whether entities remain visible.
    pub(crate) fn run_hydrated(self, mut accumulate: impl FnMut(Memory) -> bool) -> Result<(), StoreError> {
        paged_scan_loop(&self, None, &[], true, &mut accumulate)
    }

    /// Execute a paged scan with extra WHERE conditions and page-level entity hydration.
    pub(crate) fn run_with_extra_hydrated(self, extra_where: Option<&str>, extra_params: &[String], accumulate: Accumulator<'_>) -> Result<(), StoreError> {
        paged_scan_loop(&self, extra_where, extra_params, true, accumulate)
    }
}

/// Core paging loop shared by all scan operations.
///
/// Builds a `WhereClause` once from the filter, then pages through results
/// updating only the OFFSET each iteration. Extra params and filter params
/// are `String` values that are re-borrowed (not cloned) each iteration.
///
/// The loop terminates when the accumulator returns `false`, the page is not
/// full, or `MAX_SCAN_ROWS` is reached.
fn paged_scan_loop(cfg: &ScanConfig<'_>, extra_where: Option<&str>, extra_params: &[String], hydrate_entities: bool, accumulate: Accumulator<'_>) -> Result<(), StoreError> {
    let param_start = extra_params.len().saturating_add(1);
    let mut offset_val = 0_usize;
    let should_hydrate_entities = hydrate_entities || needs_entity_hydration(cfg.filter);

    // Build WhereClause and SQL once, outside the loop.
    let wc = WhereClause::from_filter(cfg.filter, cfg.caller, param_start, cfg.now);
    let where_sql = wc.to_where_sql_with_extra(extra_where);
    let next = wc.next_index();
    let sql = format!(
        "SELECT {MEMORY_COLUMNS} \
         FROM memories{where_sql} ORDER BY created_at DESC, id DESC LIMIT ?{next} OFFSET ?{}",
        next.saturating_add(1)
    );

    loop {
        // Re-borrow params each iteration (no cloning).
        let limit_i64 = usize_to_i64(cfg.page_size, "page_size")?;
        let offset_i64 = usize_to_i64(offset_val, "scan offset")?;
        let param_refs = wc.bind_params(extra_params, &limit_i64, &offset_i64);

        let mut rows = fetch_page(cfg.conn, &sql, &param_refs)?;
        if should_hydrate_entities {
            hydrate_entities_for_memories(cfg.conn, &mut rows)?;
        }

        if rows.is_empty() {
            break;
        }

        let row_count = rows.len();

        for memory in rows {
            let Some(memory) = apply_access_policy_for_filter(memory, cfg.filter, cfg.caller, cfg.now) else {
                continue;
            };
            if !accumulate(memory) {
                return Ok(());
            }
        }

        if row_count < cfg.page_size {
            break;
        }
        offset_val = offset_val.saturating_add(cfg.page_size);
        if offset_val >= MAX_SCAN_ROWS {
            break;
        }
    }
    Ok(())
}

/// Batch-hydrate entities for a slice of memories in-place.
pub(crate) fn hydrate_entities_for_memories(conn: &rusqlite::Connection, memories: &mut [Memory]) -> Result<(), StoreError> {
    if memories.is_empty() {
        return Ok(());
    }

    let ids: Vec<MemoryId> = memories.iter().map(|memory| memory.id).collect();
    let mut entity_map = hydrate_entities_batch(conn, &ids)?;
    for memory in memories {
        if let Some(entities) = entity_map.remove(&memory.id) {
            memory.entities = entities;
        }
    }

    Ok(())
}

/// Execute a single page query and deserialize results.
fn fetch_page(conn: &rusqlite::Connection, sql: &str, params: &[&dyn rusqlite::types::ToSql]) -> Result<Vec<Memory>, StoreError> {
    let mut stmt = conn.prepare(sql)?;
    stmt.query_map(params, |row| {
        row_to_memory(row).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
    })?
    .collect::<Result<Vec<_>, _>>()
    .map_err(StoreError::from)
}

/// Count memories with access-policy post-filtering in Rust, accumulating tag/agent breakdowns.
pub(crate) fn count_with_access_filter(
    conn: &rusqlite::Connection,
    filter: &MemoryFilter,
    caller: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
    top_tags_limit: usize,
) -> Result<MemoryStats, StoreError> {
    let mut total = 0_u64;
    let mut with_embedding = 0_u64;
    let mut tag_counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut agent_counts: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut memory_type_counts: std::collections::BTreeMap<MemoryType, u64> = std::collections::BTreeMap::new();
    let mut oldest: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut newest: Option<chrono::DateTime<chrono::Utc>> = None;
    let mut scope_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut superseded_count = 0_u64;

    ScanConfig::new(conn, filter, caller, now, COUNT_PAGE_SIZE).run(|memory| {
        let Some(memory) = memory.apply_access_policy(caller) else {
            return true; // skip but continue
        };
        total = total.saturating_add(1);
        if memory.has_embedding {
            with_embedding = with_embedding.saturating_add(1);
        }
        #[expect(clippy::arithmetic_side_effects, reason = "tag/agent count overflow is unreachable with u64")]
        for tag in &memory.tags {
            *tag_counts.entry(tag.clone()).or_insert(0) += 1;
        }
        #[expect(clippy::arithmetic_side_effects, reason = "agent count overflow is unreachable")]
        if let Some(agent) = &memory.provenance.source_agent {
            *agent_counts.entry(agent.clone()).or_insert(0) += 1;
        }
        // memory_type breakdown
        #[expect(clippy::arithmetic_side_effects, reason = "memory_type count overflow is unreachable with u64")]
        {
            *memory_type_counts.entry(memory.memory_type).or_insert(0) += 1;
        }
        // Track oldest/newest
        let ts = memory.created_at;
        oldest = Some(oldest.map_or(ts, |o| o.min(ts)));
        newest = Some(newest.map_or(ts, |n| n.max(ts)));
        // Track distinct scopes
        if let Some(scope) = &memory.provenance.source_conversation {
            let _inserted = scope_set.insert(scope.clone());
        }
        // Track superseded within the visible matching set.
        if memory.superseded_by.is_some() {
            superseded_count = superseded_count.saturating_add(1);
        }
        true // continue
    })?;

    // Expired count (global diagnostic)
    let now_rfc = now.to_rfc3339();
    let expired = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE expires_at IS NOT NULL AND expires_at <= ?1",
        params![now_rfc],
        sqlite_u64,
    )?;

    // Storage bytes: page_count * page_size
    let storage_bytes: Option<u64> = {
        let page_count = conn.query_row("SELECT * FROM pragma_page_count()", [], sqlite_u64)?;
        let page_size = conn.query_row("SELECT * FROM pragma_page_size()", [], sqlite_u64)?;
        Some(page_count.saturating_mul(page_size))
    };
    let scope_count = u64::try_from(scope_set.len()).unwrap_or(u64::MAX);

    let mut by_tag: Vec<(String, u64)> = tag_counts.into_iter().collect();
    by_tag.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    by_tag.truncate(top_tags_limit);

    let mut by_agent_label: Vec<(String, u64)> = agent_counts.into_iter().collect();
    by_agent_label.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    let mut by_memory_type: Vec<(MemoryType, u64)> = memory_type_counts.into_iter().collect();
    by_memory_type.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    Ok(MemoryStats {
        total,
        with_embedding,
        #[expect(clippy::arithmetic_side_effects, reason = "with_embedding <= total by construction")]
        without_embedding: total - with_embedding,
        expired,
        by_tag,
        by_agent_label,
        storage_bytes,
        oldest_memory: oldest,
        newest_memory: newest,
        scope_count,
        by_memory_type,
        superseded_count,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- RR-045: escape_like -------------------------------------------------

    #[test]
    fn escape_like_backslash() {
        assert_eq!(escape_like(r"a\b"), r"a\\b");
    }

    #[test]
    fn escape_like_percent() {
        assert_eq!(escape_like("100%"), r"100\%");
    }

    #[test]
    fn escape_like_underscore() {
        assert_eq!(escape_like("a_b"), r"a\_b");
    }

    #[test]
    fn escape_like_combined() {
        assert_eq!(escape_like(r"a%b_c\d"), r"a\%b\_c\\d");
    }

    #[test]
    fn escape_like_safe_string_unchanged() {
        let safe = "hello world 123";
        assert_eq!(escape_like(safe), safe);
    }

    // -- RR-121: matches_non_access_filter tests -----------------------------

    use chrono::{TimeZone as _, Utc};

    use crate::types::{AccessPolicy, Entity, Importance, MemoryId, MemoryType, Provenance, RedactableField, TimeRange};

    /// Build a minimal `Memory` for filter matching tests.
    fn make_memory(content: &str) -> Memory {
        Memory {
            id: MemoryId::new(),
            content: content.into(),
            tags: vec!["tag1".into(), "tag2".into()],
            provenance: Provenance {
                source_agent: Some("agent-a".into()),
                source_conversation: Some("conv-1".into()),
                origin_conversation: Some("origin-1".into()),
                source_user: None,
            },
            access_policy: AccessPolicy::Public,
            created_at: Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap(),
            updated_at: Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap(),
            expires_at: None,
            has_embedding: true,
            memory_type: MemoryType::Semantic,
            importance: Importance::default(),
            confidence: crate::types::Confidence::DEFAULT,
            impression_count: 0,
            last_impressed_at: None,
            superseded_by: None,
            activity_mass: 0.0,
            last_used_at: None,
            entities: Vec::new(),
            was_redacted: false,
        }
    }

    fn now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap()
    }

    fn redacted_memory(visible_fields: Vec<RedactableField>) -> Memory {
        let mut memory = make_memory("hello hidden content");
        memory.access_policy = AccessPolicy::Redacted { visible_fields };
        memory.entities = vec![Entity::new("Hidden Entity", "project").unwrap()];
        memory
    }

    #[test]
    fn matches_filter_empty_filter_matches() {
        let memory = make_memory("hello world");
        let filter = MemoryFilter::default();
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn redacted_hidden_fields_do_not_satisfy_filters() {
        let memory = redacted_memory(Vec::new());

        let by_tag = MemoryFilter {
            tags: Some(vec!["tag1".into()]),
            ..MemoryFilter::default()
        };
        let by_scope = MemoryFilter {
            scope: Some("conv-1".into()),
            ..MemoryFilter::default()
        };
        let by_entity = MemoryFilter {
            entity: Some("Hidden Entity".into()),
            ..MemoryFilter::default()
        };
        let by_content = MemoryFilter {
            text_search: Some("hello".into()),
            ..MemoryFilter::default()
        };

        assert!(apply_access_policy_for_filter(memory.clone(), &by_tag, Some("other"), now()).is_none());
        assert!(apply_access_policy_for_filter(memory.clone(), &by_scope, Some("other"), now()).is_none());
        assert!(apply_access_policy_for_filter(memory.clone(), &by_entity, Some("other"), now()).is_none());
        assert!(apply_access_policy_for_filter(memory, &by_content, Some("other"), now()).is_none());
    }

    #[test]
    fn redacted_visible_fields_can_satisfy_filters() {
        let filter = MemoryFilter {
            tags: Some(vec!["tag1".into()]),
            text_search: Some("hello".into()),
            ..MemoryFilter::default()
        };
        let memory = redacted_memory(vec![RedactableField::Content, RedactableField::Tags]);

        let visible = apply_access_policy_for_filter(memory, &filter, Some("other"), now());
        assert!(visible.is_some(), "visible fields should match");
        let visible = visible.unwrap_or_else(|| redacted_memory(Vec::new()));

        assert_eq!(visible.content, "hello hidden content");
        assert_eq!(visible.tags, vec!["tag1", "tag2"]);
    }

    #[test]
    fn matches_filter_text_search_case_insensitive() {
        let memory = make_memory("Hello World");
        let filter = MemoryFilter {
            text_search: Some("hello".into()),
            ..Default::default()
        }; // already lowered by normalize_filter
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_text_search_no_match() {
        let memory = make_memory("Hello World");
        let filter = MemoryFilter {
            text_search: Some("missing".into()),
            ..Default::default()
        };
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_memory_type_match() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            memory_type: Some(MemoryType::Semantic),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_memory_type_mismatch() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            memory_type: Some(MemoryType::Episodic),
            ..Default::default()
        };
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_tags_match() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            tags: Some(vec!["tag1".into()]),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_tags_no_overlap() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            tags: Some(vec!["nonexistent".into()]),
            ..Default::default()
        };
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_tags_empty_vec_matches_everything() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            tags: Some(vec![]),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_agent_label_match() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            agent_label: Some("agent-a".into()),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_agent_label_mismatch() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            agent_label: Some("agent-b".into()),
            ..Default::default()
        };
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_scope_match() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            scope: Some("conv-1".into()),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_scope_mismatch() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            scope: Some("conv-2".into()),
            ..Default::default()
        };
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_origin_scope_match() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            origin_scope: Some("origin-1".into()),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_origin_scope_falls_back_to_source() {
        let mut memory = make_memory("content");
        memory.provenance.origin_conversation = None;
        memory.provenance.source_conversation = Some("conv-1".into());
        let filter = MemoryFilter {
            origin_scope: Some("conv-1".into()),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_origin_scope_mismatch() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            origin_scope: Some("other-origin".into()),
            ..Default::default()
        };
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_scopes_any_match() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            scopes_any: Some(vec!["conv-1".into(), "conv-2".into()]),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_scopes_any_mismatch() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            scopes_any: Some(vec!["conv-x".into()]),
            ..Default::default()
        };
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_scopes_any_empty_vec_matches() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            scopes_any: Some(vec![]),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_entity_match() {
        let mut memory = make_memory("content");
        memory.entities = vec![Entity::new("Alice", "person").unwrap()];
        let filter = MemoryFilter {
            entity: Some("Alice".into()),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_entity_mismatch() {
        let mut memory = make_memory("content");
        memory.entities = vec![Entity::new("Alice", "person").unwrap()];
        let filter = MemoryFilter {
            entity: Some("Bob".into()),
            ..Default::default()
        };
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_entity_type_match() {
        let mut memory = make_memory("content");
        memory.entities = vec![Entity::new("Alice", "person").unwrap()];
        let filter = MemoryFilter {
            entity_type: Some("person".into()),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_entity_type_mismatch() {
        let mut memory = make_memory("content");
        memory.entities = vec![Entity::new("Alice", "person").unwrap()];
        let filter = MemoryFilter {
            entity_type: Some("organization".into()),
            ..Default::default()
        };
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_entity_and_entity_type_are_independent_predicates() {
        let mut memory = make_memory("content");
        memory.entities = vec![Entity::new("Alice", "person").unwrap(), Entity::new("Acme", "organization").unwrap()];
        let filter = MemoryFilter {
            entity: Some("Alice".into()),
            entity_type: Some("organization".into()),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_time_range_after() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            time_range: Some(TimeRange {
                after: Some(Utc.with_ymd_and_hms(2025, 6, 15, 11, 0, 0).unwrap()),
                before: None,
            }),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_time_range_after_too_late() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            time_range: Some(TimeRange {
                after: Some(Utc.with_ymd_and_hms(2025, 6, 15, 13, 0, 0).unwrap()),
                before: None,
            }),
            ..Default::default()
        };
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_time_range_before() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            time_range: Some(TimeRange {
                after: None,
                before: Some(Utc.with_ymd_and_hms(2025, 6, 15, 13, 0, 0).unwrap()),
            }),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_time_range_before_exclusive() {
        let memory = make_memory("content");
        // before is exclusive: memory.created_at == before should NOT match
        let filter = MemoryFilter {
            time_range: Some(TimeRange {
                after: None,
                before: Some(Utc.with_ymd_and_hms(2025, 6, 15, 12, 0, 0).unwrap()),
            }),
            ..Default::default()
        };
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_has_embedding_match() {
        let memory = make_memory("content");
        let filter = MemoryFilter {
            has_embedding: Some(true),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_has_embedding_mismatch() {
        let mut memory = make_memory("content");
        memory.has_embedding = false;
        let filter = MemoryFilter {
            has_embedding: Some(true),
            ..Default::default()
        };
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_superseded_hidden_by_default() {
        let mut memory = make_memory("content");
        memory.superseded_by = Some(MemoryId::new());
        let filter = MemoryFilter::default();
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_superseded_included_when_requested() {
        let mut memory = make_memory("content");
        memory.superseded_by = Some(MemoryId::new());
        let filter = MemoryFilter {
            include_superseded: Some(true),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_expired_memory_hidden() {
        let mut memory = make_memory("content");
        memory.expires_at = Some(Utc.with_ymd_and_hms(2025, 6, 15, 11, 0, 0).unwrap());
        let filter = MemoryFilter::default();
        // now() is 12:00, expires_at is 11:00 => expired
        assert!(!matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_not_yet_expired_visible() {
        let mut memory = make_memory("content");
        memory.expires_at = Some(Utc.with_ymd_and_hms(2025, 6, 15, 13, 0, 0).unwrap());
        let filter = MemoryFilter::default();
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    #[test]
    fn matches_filter_importance_range_via_memory_type() {
        // Importance is not a direct filter field on MemoryFilter,
        // but memory_type is — test filtering by episodic type.
        let mut memory = make_memory("content");
        memory.memory_type = MemoryType::Procedural;
        let filter = MemoryFilter {
            memory_type: Some(MemoryType::Procedural),
            ..Default::default()
        };
        assert!(matches_non_access_filter(&memory, &filter, now()));
    }

    // -- Fix regression: text_search is trimmed (#3) --------------------------

    #[test]
    fn normalize_filter_trims_text_search() {
        let filter = MemoryFilter {
            text_search: Some("  hello world  ".into()),
            ..Default::default()
        };
        let normalized = normalize_filter(filter);
        assert_eq!(normalized.text_search.as_deref(), Some("hello world"));
    }

    #[test]
    fn normalize_filter_clears_whitespace_only_text_search() {
        let filter = MemoryFilter {
            text_search: Some("   ".into()),
            ..Default::default()
        };
        let normalized = normalize_filter(filter);
        assert!(normalized.text_search.is_none());
    }

    #[test]
    fn sort_by_distance_ties_by_id_ascending() {
        let mut low = make_memory("low-id");
        low.id = "01J0000000000000000000000A".parse().unwrap();
        let mut high = make_memory("high-id");
        high.id = "01J0000000000000000000000B".parse().unwrap();
        let mut results = vec![
            SearchResult {
                memory: high,
                distance: Some(1.0_f64),
                retrieval_score: None,
                reranker_score: None,
                composite_score: None,
                score_breakdown: None,
            },
            SearchResult {
                memory: low,
                distance: Some(1.0_f64),
                retrieval_score: None,
                reranker_score: None,
                composite_score: None,
                score_breakdown: None,
            },
        ];

        sort_by_distance(&mut results);

        assert_eq!(results[0].memory.id.to_string(), "01J0000000000000000000000A");
    }
}
