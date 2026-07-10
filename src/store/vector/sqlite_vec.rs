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

/// Keep each `IN` query below SQLite's default 999 bind-parameter limit.
const FETCH_MANY_CHUNK_SIZE: usize = 900;

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
        let mut result = HashMap::new();
        for id_chunk in ids.chunks(FETCH_MANY_CHUNK_SIZE) {
            let id_strs: Vec<String> = id_chunk.iter().map(ToString::to_string).collect();
            #[expect(clippy::arithmetic_side_effects, reason = "chunk length is capped at 900, so index + 1 cannot overflow")]
            let placeholders = id_strs.iter().enumerate().map(|(i, _)| format!("?{}", i + 1)).collect::<Vec<_>>().join(", ");
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
                insert_fetched_embedding(&mut result, &id_str, &blob, self.dimensions);
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

fn insert_fetched_embedding(result: &mut EmbeddingMap, id_str: &str, blob: &[u8], dimensions: usize) {
    let Ok(id) = id_str.parse::<MemoryId>() else {
        return;
    };
    let Some(floats) = decode_embedding(blob, dimensions) else {
        tracing::warn!(
            memory_id = %id,
            blob_len = blob.len(),
            expected_bytes = dimensions.saturating_mul(size_of::<f32>()),
            "invalid embedding blob in fetch_many"
        );
        return;
    };
    if !floats.is_empty() {
        let _previous = result.insert(id, floats);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_DIMENSIONS: usize = 2;
    type TestEmbedding = (MemoryId, [f32; TEST_DIMENSIONS]);

    fn test_db() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE memory_embeddings (embedding BLOB NOT NULL);
             CREATE TABLE memory_embedding_map (
                 memory_id TEXT PRIMARY KEY,
                 vec_rowid INTEGER NOT NULL UNIQUE
             );",
        )
        .unwrap();
        db
    }

    fn insert_embeddings(index: &SqliteVecIndex, db: &Connection, entries: &[TestEmbedding]) {
        for (id, embedding) in entries {
            index.upsert(db, &id.to_string(), embedding).unwrap();
        }
    }

    #[test]
    fn fetch_many_preserves_mapping_across_chunk_boundary() {
        let db = test_db();
        let index = SqliteVecIndex::new(TEST_DIMENSIONS);
        let ids = std::iter::repeat_with(MemoryId::new).take(FETCH_MANY_CHUNK_SIZE + 1).collect::<Vec<_>>();
        let entries = [
            (ids[0], [1.0_f32, 1.5_f32]),
            (ids[FETCH_MANY_CHUNK_SIZE - 2], [2.0_f32, 2.5_f32]),
            (ids[FETCH_MANY_CHUNK_SIZE - 1], [3.0_f32, 3.5_f32]),
            (ids[FETCH_MANY_CHUNK_SIZE], [4.0_f32, 4.5_f32]),
        ];
        insert_embeddings(&index, &db, &entries);

        let at_boundary = index.fetch_many(&db, &ids[..FETCH_MANY_CHUNK_SIZE]).unwrap();
        assert_eq!(at_boundary.len(), 3);
        for (id, embedding) in &entries[..3] {
            assert_eq!(at_boundary.get(id), Some(&embedding.to_vec()));
        }

        let over_boundary = index.fetch_many(&db, &ids).unwrap();
        assert_eq!(over_boundary.len(), entries.len());
        for (id, embedding) in entries {
            assert_eq!(over_boundary.get(&id), Some(&embedding.to_vec()));
        }
    }

    #[test]
    fn fetch_many_handles_input_larger_than_sqlite_variable_limit() {
        const ID_COUNT: usize = 40_000;

        let db = test_db();
        let index = SqliteVecIndex::new(TEST_DIMENSIONS);
        let ids = std::iter::repeat_with(MemoryId::new).take(ID_COUNT).collect::<Vec<_>>();
        let entries = [
            (ids[0], [1.0_f32, 1.5_f32]),
            (ids[FETCH_MANY_CHUNK_SIZE - 1], [2.0_f32, 2.5_f32]),
            (ids[FETCH_MANY_CHUNK_SIZE], [3.0_f32, 3.5_f32]),
            (ids[FETCH_MANY_CHUNK_SIZE * 2 - 1], [4.0_f32, 4.5_f32]),
            (ids[FETCH_MANY_CHUNK_SIZE * 2], [5.0_f32, 5.5_f32]),
            (ids[ID_COUNT - 1], [6.0_f32, 6.5_f32]),
        ];
        insert_embeddings(&index, &db, &entries);

        let fetched = index.fetch_many(&db, &ids).unwrap();

        assert_eq!(fetched.len(), entries.len());
        for (id, embedding) in entries {
            assert_eq!(fetched.get(&id), Some(&embedding.to_vec()));
        }
    }
}
