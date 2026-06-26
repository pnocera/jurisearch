//! Chunk dense-embedding insertion.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkEmbeddingInsert<'a> {
    pub chunk_id: &'a str,
    pub embedding_fingerprint: &'a str,
    pub embedding_literal: &'a str,
    pub model: &'a str,
    pub dimension: usize,
}

pub fn insert_chunk_embeddings(
    postgres: &ManagedPostgres,
    embeddings: &[ChunkEmbeddingInsert<'_>],
    outbox: Option<&crate::outbox::OutboxContext<'_>>,
) -> Result<usize, StorageError> {
    if embeddings.is_empty() {
        return Ok(0);
    }

    // Build per-column arrays so the whole batch is applied set-based (one UNNEST into a temp stage,
    // one UPDATE, one upsert) instead of two statement executions per embedding.
    let chunk_ids: Vec<&str> = embeddings
        .iter()
        .map(|embedding| embedding.chunk_id)
        .collect();
    let fingerprints: Vec<&str> = embeddings
        .iter()
        .map(|embedding| embedding.embedding_fingerprint)
        .collect();
    let literals: Vec<&str> = embeddings
        .iter()
        .map(|embedding| embedding.embedding_literal)
        .collect();
    let models: Vec<&str> = embeddings.iter().map(|embedding| embedding.model).collect();
    let dimensions: Vec<i32> = embeddings
        .iter()
        .map(|embedding| {
            i32::try_from(embedding.dimension).map_err(|_| StorageError::Projection {
                message: format!(
                    "embedding dimension too large for storage on chunk `{}`: {}",
                    embedding.chunk_id, embedding.dimension
                ),
            })
        })
        .collect::<Result<_, _>>()?;

    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;

    transaction
        .batch_execute(
            "CREATE TEMP TABLE stage_chunk_embeddings ( \
                chunk_id text PRIMARY KEY, \
                embedding_fingerprint text NOT NULL, \
                embedding text NOT NULL, \
                model text NOT NULL, \
                dimension integer NOT NULL \
             ) ON COMMIT DROP;",
        )
        .map_err(StorageError::PostgresClient)?;
    transaction
        .execute(
            "INSERT INTO stage_chunk_embeddings \
                (chunk_id, embedding_fingerprint, embedding, model, dimension) \
             SELECT * FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::int[]);",
            &[&chunk_ids, &fingerprints, &literals, &models, &dimensions],
        )
        .map_err(StorageError::PostgresClient)?;

    // Batch-granular equivalent of the old per-row `updated != 1` guard: every staged chunk must
    // exist and have a NULL or matching fingerprint. A short updated count means at least one chunk
    // is missing or carries a conflicting fingerprint, so we surface a concrete offender.
    let updated = transaction
        .execute(
            "UPDATE chunks c \
             SET embedding_fingerprint = s.embedding_fingerprint \
             FROM stage_chunk_embeddings s \
             WHERE c.chunk_id = s.chunk_id \
               AND (c.embedding_fingerprint IS NULL \
                    OR c.embedding_fingerprint = s.embedding_fingerprint);",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    if updated as usize != embeddings.len() {
        let offender = transaction
            .query_opt(
                "SELECT s.chunk_id, s.embedding_fingerprint \
                 FROM stage_chunk_embeddings s \
                 LEFT JOIN chunks c ON c.chunk_id = s.chunk_id \
                 WHERE c.chunk_id IS NULL \
                    OR (c.embedding_fingerprint IS NOT NULL \
                        AND c.embedding_fingerprint <> s.embedding_fingerprint) \
                 ORDER BY s.chunk_id \
                 LIMIT 1;",
                &[],
            )
            .map_err(StorageError::PostgresClient)?;
        let message = offender
            .map(|row| {
                let chunk_id: String = row.get(0);
                let fingerprint: String = row.get(1);
                format!(
                    "chunk `{chunk_id}` is missing or has a different embedding fingerprint than `{fingerprint}`"
                )
            })
            .unwrap_or_else(|| {
                "a staged chunk is missing or has a conflicting embedding fingerprint".to_owned()
            });
        return Err(StorageError::Projection { message });
    }

    transaction
        .execute(
            "INSERT INTO chunk_embeddings \
                (chunk_id, embedding_fingerprint, embedding, model, dimension) \
             SELECT chunk_id, embedding_fingerprint, embedding::vector, model, dimension \
             FROM stage_chunk_embeddings \
             ON CONFLICT (chunk_id) DO UPDATE SET \
                embedding_fingerprint = EXCLUDED.embedding_fingerprint, \
                embedding = EXCLUDED.embedding, \
                model = EXCLUDED.model, \
                dimension = EXCLUDED.dimension;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;

    // Outbox (§5.1, plan P1): the embedding set is derived per document, so emit one `upsert` per
    // distinct document covered by this batch (not per chunk — §5.1 records scopes, not row bodies),
    // in the same transaction. The staged table is still live (ON COMMIT DROP). The embedding insert
    // also stamps `chunks.embedding_fingerprint` (a replicated parent column), so a paired
    // document-scoped `chunks` upsert is emitted too, or the parent fingerprint change goes uncaptured.
    if let Some(ctx) = outbox {
        let rows = transaction
            .query(
                "SELECT DISTINCT d.document_id, d.corpus \
                 FROM stage_chunk_embeddings s \
                 JOIN chunks c ON c.chunk_id = s.chunk_id \
                 JOIN documents d ON d.document_id = c.document_id \
                 ORDER BY d.document_id;",
                &[],
            )
            .map_err(StorageError::PostgresClient)?;
        for row in rows {
            let document_id: String = row.get("document_id");
            let corpus: String = row.get("corpus");
            for table in ["chunks", "chunk_embeddings"] {
                crate::outbox::emit_change(
                    &mut transaction,
                    ctx,
                    &crate::outbox::OutboxEvent::scope(
                        &corpus,
                        table,
                        jurisearch_package::event::EventKind::Upsert,
                        crate::outbox::scope_kind::DOCUMENT,
                        &document_id,
                    ),
                )?;
            }
        }
    }

    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(embeddings.len())
}
