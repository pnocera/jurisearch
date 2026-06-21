use serde_json::json;

use crate::runtime::{ManagedPostgres, StorageError};

pub const DENSE_VECTOR_INDEX_NAME: &str = "chunk_embeddings_embedding_ivfflat_idx";
pub const DENSE_VECTOR_DIMENSION: i32 = 1024;

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

pub fn finalize_dense_rebuild(
    postgres: &ManagedPostgres,
    spec: &DenseRebuildSpec<'_>,
) -> Result<DenseRebuildReport, StorageError> {
    validate_dense_spec(spec)?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
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

    transaction
        .execute(
            "UPDATE chunks SET embedding_fingerprint = $1;",
            &[&spec.embedding_fingerprint],
        )
        .map_err(StorageError::PostgresClient)?;
    transaction
        .batch_execute(&format!(
            "DROP INDEX IF EXISTS {index_name}; \
             CREATE INDEX {index_name} \
             ON chunk_embeddings USING ivfflat (embedding vector_l2_ops) \
             WITH (lists = {lists}); \
             ANALYZE chunks; \
             ANALYZE chunk_embeddings;",
            index_name = DENSE_VECTOR_INDEX_NAME,
            lists = spec.index_lists
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
            "lists": spec.index_lists
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
        index_lists: spec.index_lists,
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
    if spec.index_lists == 0 {
        return Err(StorageError::DenseRebuild {
            message: "index_lists must be at least 1".to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DENSE_VECTOR_DIMENSION, DenseRebuildSpec, validate_dense_spec};

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

        let invalid_lists = DenseRebuildSpec {
            index_lists: 0,
            ..valid
        };
        assert!(validate_dense_spec(&invalid_lists).is_err());

        let inconsistent_fingerprint = DenseRebuildSpec {
            embedding_fingerprint: "other:1024:normalize:true",
            ..valid
        };
        assert!(validate_dense_spec(&inconsistent_fingerprint).is_err());
    }
}
