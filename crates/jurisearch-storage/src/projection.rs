use jurisearch_ingest::legi::{CanonicalDocument, CanonicalGraphEdge};

use crate::runtime::{ManagedPostgres, StorageError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalInsertReport {
    pub documents: usize,
    pub chunks: usize,
    pub publisher_edges: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkEmbeddingInsert<'a> {
    pub chunk_id: &'a str,
    pub embedding_fingerprint: &'a str,
    pub embedding_literal: &'a str,
    pub model: &'a str,
    pub dimension: usize,
}

pub fn insert_legi_documents(
    postgres: &ManagedPostgres,
    documents: &[CanonicalDocument],
    chunk_embedding_fingerprint: Option<&str>,
) -> Result<CanonicalInsertReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;

    let document_statement = transaction
        .prepare(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, version_group, citation, title, body, \
                 valid_from, valid_to, valid_to_raw, source_url, source_payload_hash, canonical_json) \
             VALUES \
                ($1, $2, $3, $4, $5, $6, $7, $8, \
                 $9::text::date, $10::text::date, $11, $12, $13, $14::text::jsonb) \
             ON CONFLICT (document_id) DO UPDATE SET \
                source = EXCLUDED.source, \
                kind = EXCLUDED.kind, \
                source_uid = EXCLUDED.source_uid, \
                version_group = EXCLUDED.version_group, \
                citation = EXCLUDED.citation, \
                title = EXCLUDED.title, \
                body = EXCLUDED.body, \
                valid_from = EXCLUDED.valid_from, \
                valid_to = EXCLUDED.valid_to, \
                valid_to_raw = EXCLUDED.valid_to_raw, \
                source_url = EXCLUDED.source_url, \
                source_payload_hash = EXCLUDED.source_payload_hash, \
                canonical_json = EXCLUDED.canonical_json, \
                updated_at = now();",
        )
        .map_err(StorageError::PostgresClient)?;
    let chunk_statement = transaction
        .prepare(
            "INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, chunk_kind, source_fields, \
                 source_payload_hash, chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ($1, $2, $3, $4, $5, $6::text::jsonb, $7, $8, $9) \
             ON CONFLICT (chunk_id) DO UPDATE SET \
                document_id = EXCLUDED.document_id, \
                chunk_index = EXCLUDED.chunk_index, \
                body = EXCLUDED.body, \
                chunk_kind = EXCLUDED.chunk_kind, \
                source_fields = EXCLUDED.source_fields, \
                source_payload_hash = EXCLUDED.source_payload_hash, \
                chunk_builder_version = EXCLUDED.chunk_builder_version, \
                embedding_fingerprint = EXCLUDED.embedding_fingerprint;",
        )
        .map_err(StorageError::PostgresClient)?;
    let edge_statement = transaction
        .prepare(
            "INSERT INTO graph_edges \
                (edge_id, from_document_id, to_document_id, edge_kind, edge_source, payload) \
             VALUES ($1, $2, $3, $4, $5, $6::text::jsonb) \
             ON CONFLICT (edge_id) DO UPDATE SET \
                from_document_id = EXCLUDED.from_document_id, \
                to_document_id = EXCLUDED.to_document_id, \
                edge_kind = EXCLUDED.edge_kind, \
                edge_source = EXCLUDED.edge_source, \
                payload = EXCLUDED.payload;",
        )
        .map_err(StorageError::PostgresClient)?;

    let mut chunks = 0usize;
    let mut publisher_edges = 0usize;
    for document in documents {
        document
            .validate()
            .map_err(|error| StorageError::Projection {
                message: format!(
                    "canonical document `{}` failed validation before storage projection: {error}",
                    document.document_id
                ),
            })?;
        let canonical_json = serde_json::to_string(document)?;
        transaction
            .execute(
                &document_statement,
                &[
                    &document.document_id,
                    &document.source,
                    &document.kind,
                    &document.source_uid,
                    &document.version_group,
                    &document.citation,
                    &document.title,
                    &document.body,
                    &document.valid_from,
                    &document.valid_to,
                    &document.valid_to_raw,
                    &document.source_url,
                    &document.source_payload_hash,
                    &canonical_json,
                ],
            )
            .map_err(StorageError::PostgresClient)?;

        for chunk in &document.chunks {
            let source_fields = serde_json::to_string(&chunk.source_fields)?;
            let chunk_index =
                i32::try_from(chunk.chunk_index).map_err(|_| StorageError::Projection {
                    message: format!(
                        "chunk `{}` has chunk_index too large for storage: {}",
                        chunk.chunk_id, chunk.chunk_index
                    ),
                })?;
            transaction
                .execute(
                    &chunk_statement,
                    &[
                        &chunk.chunk_id,
                        &chunk.document_id,
                        &chunk_index,
                        &chunk.body,
                        &chunk.chunk_kind,
                        &source_fields,
                        &chunk.source_payload_hash,
                        &chunk.chunk_builder_version,
                        &chunk_embedding_fingerprint,
                    ],
                )
                .map_err(StorageError::PostgresClient)?;
            chunks += 1;
        }

        for edge in &document.publisher_edges {
            insert_publisher_edge(&mut transaction, &edge_statement, edge)?;
            publisher_edges += 1;
        }
    }

    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(CanonicalInsertReport {
        documents: documents.len(),
        chunks,
        publisher_edges,
    })
}

pub fn insert_chunk_embeddings(
    postgres: &ManagedPostgres,
    embeddings: &[ChunkEmbeddingInsert<'_>],
) -> Result<usize, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    let statement = transaction
        .prepare(
            "INSERT INTO chunk_embeddings \
                (chunk_id, embedding_fingerprint, embedding, model, dimension) \
             VALUES ($1, $2, $3::text::vector, $4, $5) \
             ON CONFLICT (chunk_id) DO UPDATE SET \
                embedding_fingerprint = EXCLUDED.embedding_fingerprint, \
                embedding = EXCLUDED.embedding, \
                model = EXCLUDED.model, \
                dimension = EXCLUDED.dimension;",
        )
        .map_err(StorageError::PostgresClient)?;
    let chunk_fingerprint_statement = transaction
        .prepare(
            "UPDATE chunks \
             SET embedding_fingerprint = $2 \
             WHERE chunk_id = $1 \
               AND (embedding_fingerprint IS NULL OR embedding_fingerprint = $2);",
        )
        .map_err(StorageError::PostgresClient)?;

    for embedding in embeddings {
        let dimension =
            i32::try_from(embedding.dimension).map_err(|_| StorageError::Projection {
                message: format!(
                    "embedding dimension too large for storage on chunk `{}`: {}",
                    embedding.chunk_id, embedding.dimension
                ),
            })?;
        let updated = transaction
            .execute(
                &chunk_fingerprint_statement,
                &[&embedding.chunk_id, &embedding.embedding_fingerprint],
            )
            .map_err(StorageError::PostgresClient)?;
        if updated != 1 {
            return Err(StorageError::Projection {
                message: format!(
                    "chunk `{}` is missing or has a different embedding fingerprint than `{}`",
                    embedding.chunk_id, embedding.embedding_fingerprint
                ),
            });
        }
        transaction
            .execute(
                &statement,
                &[
                    &embedding.chunk_id,
                    &embedding.embedding_fingerprint,
                    &embedding.embedding_literal,
                    &embedding.model,
                    &dimension,
                ],
            )
            .map_err(StorageError::PostgresClient)?;
    }

    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(embeddings.len())
}

fn insert_publisher_edge(
    transaction: &mut postgres::Transaction<'_>,
    statement: &postgres::Statement,
    edge: &CanonicalGraphEdge,
) -> Result<(), StorageError> {
    let payload = serde_json::to_string(edge)?;
    transaction
        .execute(
            statement,
            &[
                &edge.edge_id,
                &edge.from_document_id,
                &edge.to_document_id,
                &edge.relation,
                &edge.edge_source,
                &payload,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}
