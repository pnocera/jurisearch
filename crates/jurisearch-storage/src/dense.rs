use serde_json::json;

use crate::runtime::{ManagedPostgres, StorageError};

pub const DENSE_VECTOR_INDEX_NAME: &str = "chunk_embeddings_embedding_ivfflat_idx";
pub const DENSE_VECTOR_DIMENSION: i32 = 1024;

/// pgvector's IVFFlat `lists` heuristic for a corpus of `rows` indexed vectors: `rows / 1000` up to
/// 1M rows, then `sqrt(rows)` beyond, clamped to at least 1. Used when the embed CLI passes
/// `--index-lists 0` (auto). It replaces the previous fixed `lists = 32` default, which badly
/// under-partitioned multi-million-row corpora (4.7M vectors / 32 lists ≈ 147k vectors per probe →
/// seconds-long dense scans); auto scales it to ≈2168 lists for the same corpus.
pub fn recommended_ivfflat_lists(rows: i64) -> u32 {
    let rows = rows.max(0) as f64;
    let lists = if rows <= 1_000_000.0 {
        rows / 1000.0
    } else {
        rows.sqrt()
    };
    lists.round().clamp(1.0, f64::from(u32::MAX)) as u32
}

/// Recommended `ivfflat.probes` for an index built with `lists` partitions: `sqrt(lists)`, clamped to
/// the `--probes` range `[1, 4096]`. Persisted as `vector_index.default_probes` in the index manifest at
/// build time so the query path can scale probes to a corpus-sized index — a fixed `probes = 4` over
/// ~2168 lists probes ~0.2% of the clusters, collapsing recall.
pub fn recommended_probes(lists: u32) -> u32 {
    f64::from(lists).sqrt().round().clamp(1.0, 4096.0) as u32
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DenseRebuildSpec<'a> {
    pub embedding_fingerprint: &'a str,
    pub model: &'a str,
    pub dimension: i32,
    pub normalize: bool,
    pub provisional: bool,
    pub reembeddable: bool,
    pub index_lists: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenseRebuildReport {
    pub chunks: i64,
    pub embeddings: i64,
    pub embedding_fingerprint: String,
    pub index_name: String,
    pub index_lists: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkEmbeddingInput {
    pub chunk_id: String,
    pub embedding_text: String,
}

/// Thin [`ManagedPostgres`] shim over [`load_chunk_embedding_inputs_with_client`] (work/10 M1-B S1):
/// open a fresh client and delegate. Behavior is unchanged for existing callers.
pub fn load_chunk_embedding_inputs(
    postgres: &ManagedPostgres,
    embedding_fingerprint: &str,
    model: &str,
    dimension: i32,
    limit: Option<u32>,
) -> Result<Vec<ChunkEmbeddingInput>, StorageError> {
    let mut client = postgres.client()?;
    load_chunk_embedding_inputs_with_client(
        &mut client,
        embedding_fingerprint,
        model,
        dimension,
        limit,
    )
}

/// Client-source variant of [`load_chunk_embedding_inputs`] (work/10 M1-B S1): runs the same query over
/// a borrowed client so the producer can read against an external PostgreSQL.
pub fn load_chunk_embedding_inputs_with_client(
    client: &mut postgres::Client,
    embedding_fingerprint: &str,
    model: &str,
    dimension: i32,
    limit: Option<u32>,
) -> Result<Vec<ChunkEmbeddingInput>, StorageError> {
    let rows = if let Some(limit) = limit {
        let limit = i64::from(limit);
        client
            .query(
                "SELECT c.chunk_id, c.body, c.contextualized_body \
                 FROM chunks c \
                 LEFT JOIN chunk_embeddings ce ON ce.chunk_id = c.chunk_id \
                 WHERE ce.chunk_id IS NULL \
                    OR ce.embedding_fingerprint <> $1 \
                    OR ce.model <> $2 \
                    OR ce.dimension <> $3 \
                 ORDER BY c.document_id, c.chunk_index, c.chunk_id \
                 LIMIT $4;",
                &[&embedding_fingerprint, &model, &dimension, &limit],
            )
            .map_err(StorageError::PostgresClient)?
    } else {
        client
            .query(
                "SELECT c.chunk_id, c.body, c.contextualized_body \
                 FROM chunks c \
                 LEFT JOIN chunk_embeddings ce ON ce.chunk_id = c.chunk_id \
                 WHERE ce.chunk_id IS NULL \
                    OR ce.embedding_fingerprint <> $1 \
                    OR ce.model <> $2 \
                    OR ce.dimension <> $3 \
                 ORDER BY c.document_id, c.chunk_index, c.chunk_id;",
                &[&embedding_fingerprint, &model, &dimension],
            )
            .map_err(StorageError::PostgresClient)?
    };

    Ok(rows
        .into_iter()
        .map(|row| {
            let chunk_id: String = row.get(0);
            let body: String = row.get(1);
            let contextualized_body: Option<String> = row.get(2);
            let embedding_text = contextualized_body
                .filter(|text| !text.trim().is_empty())
                .unwrap_or(body);

            ChunkEmbeddingInput {
                chunk_id,
                embedding_text,
            }
        })
        .collect())
}

/// Thin [`ManagedPostgres`] shim over [`finalize_dense_rebuild_with_client`] (work/10 M1-B S1).
pub fn finalize_dense_rebuild(
    postgres: &ManagedPostgres,
    spec: &DenseRebuildSpec<'_>,
    outbox: Option<&crate::outbox::OutboxContext<'_>>,
) -> Result<DenseRebuildReport, StorageError> {
    let mut client = postgres.client()?;
    finalize_dense_rebuild_with_client(&mut client, spec, outbox)
}

/// Client-source variant of [`finalize_dense_rebuild`] (work/10 M1-B S1): runs the identical
/// validate → coverage-check → fingerprint-stamp → ivfflat-rebuild → manifest transaction over a
/// borrowed client, preserving the single-transaction boundary.
pub fn finalize_dense_rebuild_with_client(
    client: &mut postgres::Client,
    spec: &DenseRebuildSpec<'_>,
    outbox: Option<&crate::outbox::OutboxContext<'_>>,
) -> Result<DenseRebuildReport, StorageError> {
    validate_dense_spec(spec)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;

    let chunks: i64 = transaction
        .query_one("SELECT count(*) FROM chunks;", &[])
        .map_err(StorageError::PostgresClient)?
        .get(0);
    if chunks == 0 {
        return Err(StorageError::DenseRebuild {
            message: "cannot finalize dense rebuild for an empty chunk corpus".to_owned(),
        });
    }
    let embeddings: i64 = transaction
        .query_one(
            "SELECT count(*) \
             FROM chunk_embeddings \
             WHERE embedding_fingerprint = $1 \
               AND model = $2 \
               AND dimension = $3;",
            &[&spec.embedding_fingerprint, &spec.model, &spec.dimension],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    let missing: i64 = transaction
        .query_one(
            "SELECT count(*) \
             FROM chunks c \
             LEFT JOIN chunk_embeddings ce ON ce.chunk_id = c.chunk_id \
             WHERE ce.chunk_id IS NULL \
                OR ce.embedding_fingerprint <> $1 \
                OR ce.model <> $2 \
                OR ce.dimension <> $3;",
            &[&spec.embedding_fingerprint, &spec.model, &spec.dimension],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    if missing != 0 {
        return Err(StorageError::DenseRebuild {
            message: format!(
                "{missing} chunks are missing embeddings for fingerprint `{}`",
                spec.embedding_fingerprint
            ),
        });
    }

    // `index_lists == 0` means auto: scale the partition count to the indexed-vector count so the ANN
    // index stays well-partitioned as the corpus grows (instead of the old fixed 32). An explicit
    // non-zero `--index-lists` is honored verbatim. `default_probes` is derived from the lists actually
    // built and persisted for the query path (Fix #2).
    let effective_lists = if spec.index_lists == 0 {
        recommended_ivfflat_lists(embeddings)
    } else {
        spec.index_lists
    };
    let default_probes = recommended_probes(effective_lists);

    // Stamp only the chunks whose parent fingerprint actually changes, and emit one document-scoped
    // `chunks` upsert per affected document in this transaction (§5.1, P1): `embedding_fingerprint`
    // is a replicated column, so a finalize that changes it must be captured. In the common path the
    // insert writer already stamped these rows, so this updates nothing and emits nothing.
    let stamped = transaction
        .query(
            "UPDATE chunks SET embedding_fingerprint = $1 \
             WHERE embedding_fingerprint IS DISTINCT FROM $1 \
             RETURNING document_id;",
            &[&spec.embedding_fingerprint],
        )
        .map_err(StorageError::PostgresClient)?;
    if let Some(ctx) = outbox {
        let documents: Vec<String> = stamped
            .iter()
            .map(|row| row.get::<_, String>("document_id"))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !documents.is_empty() {
            let corpus_rows = transaction
                .query(
                    "SELECT document_id, corpus FROM documents \
                     WHERE document_id = ANY($1) ORDER BY document_id;",
                    &[&documents],
                )
                .map_err(StorageError::PostgresClient)?;
            for row in corpus_rows {
                let document_id: String = row.get("document_id");
                let corpus: String = row.get("corpus");
                crate::outbox::emit_change(
                    &mut transaction,
                    ctx,
                    &crate::outbox::OutboxEvent::scope(
                        &corpus,
                        "chunks",
                        jurisearch_package::event::EventKind::Upsert,
                        crate::outbox::scope_kind::DOCUMENT,
                        &document_id,
                    ),
                )?;
            }
        }
    }
    transaction
        .batch_execute(&format!(
            "DROP INDEX IF EXISTS {index_name}; \
             CREATE INDEX {index_name} \
             ON chunk_embeddings USING ivfflat (embedding vector_l2_ops) \
             WITH (lists = {lists}); \
             ANALYZE chunks; \
             ANALYZE chunk_embeddings;",
            index_name = DENSE_VECTOR_INDEX_NAME,
            lists = effective_lists
        ))
        .map_err(StorageError::PostgresClient)?;

    let manifest = json!({
        "embedding_fingerprint": spec.embedding_fingerprint,
        "model": spec.model,
        "dimension": spec.dimension,
        "normalize": spec.normalize,
        "provisional": spec.provisional,
        "reembeddable": spec.reembeddable,
        "vector_index": {
            "name": DENSE_VECTOR_INDEX_NAME,
            "method": "ivfflat",
            "operator_class": "vector_l2_ops",
            "lists": effective_lists,
            "default_probes": default_probes
        },
        "coverage": {
            "chunks": chunks,
            "embeddings": embeddings
        }
    })
    .to_string();
    transaction
        .execute(
            "INSERT INTO index_manifest(key, value, updated_at) \
             VALUES ('embedding', $1::text::jsonb, now()) \
             ON CONFLICT (key) DO UPDATE \
             SET value = EXCLUDED.value, \
                 updated_at = EXCLUDED.updated_at;",
            &[&manifest],
        )
        .map_err(StorageError::PostgresClient)?;
    transaction.commit().map_err(StorageError::PostgresClient)?;

    Ok(DenseRebuildReport {
        chunks,
        embeddings,
        embedding_fingerprint: spec.embedding_fingerprint.to_owned(),
        index_name: DENSE_VECTOR_INDEX_NAME.to_owned(),
        index_lists: effective_lists,
    })
}

fn validate_dense_spec(spec: &DenseRebuildSpec<'_>) -> Result<(), StorageError> {
    if spec.embedding_fingerprint.trim().is_empty() {
        return Err(StorageError::DenseRebuild {
            message: "embedding_fingerprint must not be empty".to_owned(),
        });
    }
    if spec.model.trim().is_empty() {
        return Err(StorageError::DenseRebuild {
            message: "model must not be empty".to_owned(),
        });
    }
    if spec.dimension != DENSE_VECTOR_DIMENSION {
        return Err(StorageError::DenseRebuild {
            message: format!(
                "dense rebuild dimension must match schema vector({}), got {}",
                DENSE_VECTOR_DIMENSION, spec.dimension
            ),
        });
    }
    let expected_fingerprint = format!(
        "{}:{}:normalize:{}",
        spec.model, spec.dimension, spec.normalize
    );
    if spec.embedding_fingerprint != expected_fingerprint {
        return Err(StorageError::DenseRebuild {
            message: format!(
                "embedding_fingerprint `{}` does not match model/dimension/normalize spec `{expected_fingerprint}`",
                spec.embedding_fingerprint
            ),
        });
    }
    // `index_lists == 0` is valid: it requests auto-scaling at finalize time (see
    // `recommended_ivfflat_lists`). Any explicit value is honored as-is.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        DENSE_VECTOR_DIMENSION, DenseRebuildSpec, recommended_ivfflat_lists, recommended_probes,
        validate_dense_spec,
    };

    #[test]
    fn dense_spec_validation_rejects_invalid_inputs() {
        let valid = DenseRebuildSpec {
            embedding_fingerprint: "bge-m3:1024:normalize:true",
            model: "bge-m3",
            dimension: DENSE_VECTOR_DIMENSION,
            normalize: true,
            provisional: true,
            reembeddable: true,
            index_lists: 1,
        };
        assert!(validate_dense_spec(&valid).is_ok());

        let invalid_dimension = DenseRebuildSpec {
            dimension: 768,
            ..valid
        };
        assert!(validate_dense_spec(&invalid_dimension).is_err());

        // `index_lists == 0` is now valid: it requests auto-scaling at finalize time.
        let auto_lists = DenseRebuildSpec {
            index_lists: 0,
            ..valid
        };
        assert!(validate_dense_spec(&auto_lists).is_ok());

        let inconsistent_fingerprint = DenseRebuildSpec {
            embedding_fingerprint: "other:1024:normalize:true",
            ..valid
        };
        assert!(validate_dense_spec(&inconsistent_fingerprint).is_err());
    }

    #[test]
    fn recommended_ivfflat_lists_follows_pgvector_heuristic() {
        // Degenerate / tiny corpora never drop below a single partition.
        assert_eq!(recommended_ivfflat_lists(0), 1);
        assert_eq!(recommended_ivfflat_lists(-5), 1);
        assert_eq!(recommended_ivfflat_lists(1), 1);
        assert_eq!(recommended_ivfflat_lists(500), 1);
        // rows/1000 up to 1M (rounded).
        assert_eq!(recommended_ivfflat_lists(1_500), 2);
        assert_eq!(recommended_ivfflat_lists(32_000), 32);
        assert_eq!(recommended_ivfflat_lists(1_000_000), 1_000);
        // sqrt(rows) beyond 1M — the production corpus lands near the bear-measured 2168.
        assert_eq!(recommended_ivfflat_lists(4_701_354), 2168);
    }

    #[test]
    fn recommended_probes_is_sqrt_lists_clamped() {
        assert_eq!(recommended_probes(0), 1);
        assert_eq!(recommended_probes(1), 1);
        assert_eq!(recommended_probes(2168), 47);
        // Clamped to the `--probes` ceiling even for an extreme list count.
        assert_eq!(recommended_probes(u32::MAX), 4096);
    }
}
