//! SQLite `sqlite-vec` implementation of the vector index port.

use std::collections::HashMap;

use rusqlite::{Connection, params};
use zerocopy::IntoBytes as _;

use super::{VectorBatch, VectorHit, VectorIndex, validate_embedding_vector};
use crate::{
    error::StoreError,
    store::{EmbeddingMap, query::usize_to_i64, schema::check_dimension_mismatch},
    types::MemoryId,
};

/// sqlite-vec backed vector index for the SQLite store.
#[derive(Debug, Clone)]
pub(crate) struct SqliteVecIndex {
    dimensions: usize,
}

impl SqliteVecIndex {
    /// Create a SQLite vector index adapter for the configured dimensions.
    pub(crate) const fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }

    fn validate_dimensions(&self, embedding: &[f32]) -> Result<(), StoreError> {
        validate_embedding_vector(embedding, self.dimensions)
    }
}

impl VectorIndex<Connection> for SqliteVecIndex {
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn init_schema(&self, db: &Connection) -> Result<(), StoreError> {
        check_dimension_mismatch(db, self.dimensions)?;

        // SAFETY(RR-097): `dimensions` is a `usize` from validated config.
        // Interpolating it into the DDL string is safe because `usize::fmt`
        // produces only ASCII digits, so SQL injection is impossible.
        let vec_ddl = format!(
            "CREATE VIRTUAL TABLE IF NOT EXISTS memory_embeddings USING vec0(
                embedding float[{}]
            );",
            self.dimensions
        );
        db.execute_batch(&vec_ddl)?;
        Ok(())
    }

    fn upsert(&self, db: &Connection, memory_id: &str, embedding: &[f32]) -> Result<(), StoreError> {
        self.validate_dimensions(embedding)?;
        let emb_bytes: &[u8] = embedding.as_bytes();

        // Insert first. If this runs inside a transaction, failures below roll
        // back the new vector and preserve the previous mapping.
        #[expect(unused_results, reason = "INSERT row count is always 1")]
        db.execute("INSERT INTO memory_embeddings (embedding) VALUES (?1)", params![emb_bytes])?;
        let new_vec_rowid = db.last_insert_rowid();

        #[expect(unused_results, reason = "DELETE row count may be 0 when no prior embedding exists")]
        db.execute("DELETE FROM memory_embedding_map WHERE memory_id = ?1", params![memory_id])?;

        #[expect(unused_results, reason = "INSERT row count is always 1")]
        db.execute("INSERT INTO memory_embedding_map (memory_id, vec_rowid) VALUES (?1, ?2)", params![memory_id, new_vec_rowid])?;
        Ok(())
    }

    fn delete(&self, db: &Connection, memory_id: &str) -> Result<(), StoreError> {
        #[expect(unused_results, reason = "DELETE row count not needed; operation is idempotent")]
        db.execute("DELETE FROM memory_embedding_map WHERE memory_id = ?1", params![memory_id])?;
        Ok(())
    }

    fn search_batch(&self, db: &Connection, embedding: &[f32], limit: usize) -> Result<VectorBatch, StoreError> {
        if limit == 0 {
            return Ok(VectorBatch {
                hits: Vec::new(),
                returned_count: 0,
            });
        }
        self.validate_dimensions(embedding)?;
        let emb_bytes: &[u8] = embedding.as_bytes();
        let mut stmt = db.prepare(
            "SELECT em.memory_id, knn.distance \
             FROM ( \
               SELECT rowid, distance \
               FROM memory_embeddings \
               WHERE embedding MATCH ?1 \
               ORDER BY distance \
               LIMIT ?2 \
             ) knn \
             JOIN memory_embedding_map em ON em.vec_rowid = knn.rowid \
             ORDER BY knn.distance",
        )?;
        let rows: Vec<(String, f64)> = stmt
            .query_map(params![emb_bytes, usize_to_i64(limit, "vector candidate limit")?], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let returned_count = rows.len();
        let hits = rows.into_iter().filter_map(parse_vector_hit).collect();
        Ok(VectorBatch { hits, returned_count })
    }

    fn neighbors(&self, db: &Connection, embedding: &[f32], max_l2_distance: f64, limit: usize) -> Result<Vec<VectorHit>, StoreError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        self.validate_dimensions(embedding)?;
        let emb_bytes: &[u8] = embedding.as_bytes();
        let fetch_limit = limit.saturating_mul(2).min(crate::store::query::MAX_VEC_CANDIDATES);
        let mut stmt = db.prepare(
            "SELECT em.memory_id, knn.distance \
             FROM ( \
               SELECT rowid, distance \
               FROM memory_embeddings \
               WHERE embedding MATCH ?1 AND k = ?2 \
             ) knn \
             JOIN memory_embedding_map em ON em.vec_rowid = knn.rowid \
             JOIN memories m ON m.id = em.memory_id \
             WHERE m.superseded_by IS NULL \
             ORDER BY knn.distance",
        )?;
        let rows: Vec<(String, f64)> = stmt
            .query_map(params![emb_bytes, usize_to_i64(fetch_limit, "neighbor limit")?], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|(_, distance)| *distance <= max_l2_distance)
            .filter_map(parse_vector_hit)
            .take(limit)
            .collect())
    }

    fn fetch_many(&self, db: &Connection, ids: &[MemoryId]) -> Result<EmbeddingMap, StoreError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let id_strs: Vec<String> = ids.iter().map(ToString::to_string).collect();
        let mut result = HashMap::new();
        #[expect(clippy::arithmetic_side_effects, reason = "enumerate index + 1 cannot overflow for realistic ID counts")]
        let placeholders: String = id_strs.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT em.memory_id, e.embedding FROM memory_embedding_map em \
             JOIN memory_embeddings e ON e.rowid = em.vec_rowid \
             WHERE em.memory_id IN ({placeholders})"
        );
        let mut stmt = db.prepare(&sql)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = id_strs.iter().map(coerce_to_sql).collect();
        let rows = stmt.query_map(params.as_slice(), |row| {
            let id_str: String = row.get(0)?;
            let blob: Vec<u8> = row.get(1)?;
            Ok((id_str, blob))
        })?;
        for row in rows {
            let (id_str, blob) = row?;
            let Ok(id) = id_str.parse::<MemoryId>() else { continue };
            let Some(floats) = decode_embedding(&blob, self.dimensions) else {
                tracing::warn!(
                    memory_id = %id,
                    blob_len = blob.len(),
                    expected_bytes = self.dimensions.saturating_mul(size_of::<f32>()),
                    "invalid embedding blob in fetch_many"
                );
                continue;
            };
            if !floats.is_empty() {
                let _prev = result.insert(id, floats);
            }
        }
        Ok(result)
    }
}

fn parse_vector_hit((id_str, distance): (String, f64)) -> Option<VectorHit> {
    match id_str.parse::<MemoryId>() {
        Ok(memory_id) => Some(VectorHit { memory_id, distance }),
        Err(e) => {
            tracing::warn!(memory_id = id_str, error = %e, "invalid memory ID in vector index");
            None
        }
    }
}

/// Decode a raw byte blob into an expected-dimension `f32` embedding vector.
#[expect(clippy::expect_used, reason = "chunks_exact guarantees each chunk is exactly 4 bytes")]
#[expect(clippy::host_endian_bytes, reason = "embeddings are stored as native-endian f32 via zerocopy::IntoBytes")]
fn decode_embedding(blob: &[u8], dimensions: usize) -> Option<Vec<f32>> {
    let expected_bytes = dimensions.saturating_mul(size_of::<f32>());
    if blob.len() != expected_bytes {
        return None;
    }
    let chunks = blob.chunks_exact(size_of::<f32>());
    if !chunks.remainder().is_empty() {
        return None;
    }
    Some(chunks.map(|chunk| f32::from_ne_bytes(chunk.try_into().expect("chunks_exact guarantees 4 bytes"))).collect())
}

fn coerce_to_sql(s: &String) -> &dyn rusqlite::types::ToSql {
    s
}
