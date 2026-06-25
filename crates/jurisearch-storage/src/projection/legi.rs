//! LEGI document projection + prepared statements.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CanonicalInsertReport {
    pub documents: usize,
    pub chunks: usize,
    pub publisher_edges: usize,
    /// Lower-trust body-parsed citation edges (`edge_source = "inferred"`). Always 0 for LEGI.
    pub inferred_edges: usize,
}

pub fn insert_legi_documents(
    postgres: &ManagedPostgres,
    documents: &[CanonicalDocument],
    chunk_embedding_fingerprint: Option<&str>,
) -> Result<CanonicalInsertReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    let report = insert_legi_documents_with_client(
        &mut transaction,
        documents,
        chunk_embedding_fingerprint,
    )?;
    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(report)
}

/// LEGI document/chunk/edge upsert statements, prepared once and reused across a whole ingest batch
/// instead of being re-parsed per member (an archive batch holds up to ~128 members).
pub struct LegiProjectionStatements {
    pub(super) document: postgres::Statement,
    pub(super) chunk: postgres::Statement,
    pub(super) edge: postgres::Statement,
}

/// Prepare the three LEGI projection statements on `client` (typically once per ingest transaction).
pub fn prepare_legi_projection_statements<C: GenericClient>(
    client: &mut C,
) -> Result<LegiProjectionStatements, StorageError> {
    let document = client
        .prepare(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, version_group, citation, title, body, \
                 valid_from, valid_to, valid_to_raw, source_url, source_payload_hash, \
                 hierarchy_path, canonical_json) \
             VALUES \
                ($1, $2, $3, $4, $5, $6, $7, $8, \
                 $9::text::date, $10::text::date, $11, $12, $13, $14::text::jsonb, \
                 $15::text::jsonb) \
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
                hierarchy_path = EXCLUDED.hierarchy_path, \
                canonical_json = EXCLUDED.canonical_json, \
                updated_at = now();",
        )
        .map_err(StorageError::PostgresClient)?;
    let chunk = client
        .prepare(
            "INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, chunk_kind, \
                 chunking, boundary, source_fields, source_payload_hash, \
                 chunk_builder_version, hierarchy_path, embedding_fingerprint) \
             VALUES \
                ($1, $2, $3, $4, $5, $6, \
                 $7, $8, $9::text::jsonb, $10, $11, $12::text::jsonb, $13) \
             ON CONFLICT (chunk_id) DO UPDATE SET \
                document_id = EXCLUDED.document_id, \
                chunk_index = EXCLUDED.chunk_index, \
                body = EXCLUDED.body, \
                contextualized_body = EXCLUDED.contextualized_body, \
                chunk_kind = EXCLUDED.chunk_kind, \
                chunking = EXCLUDED.chunking, \
                boundary = EXCLUDED.boundary, \
                source_fields = EXCLUDED.source_fields, \
                source_payload_hash = EXCLUDED.source_payload_hash, \
                chunk_builder_version = EXCLUDED.chunk_builder_version, \
                hierarchy_path = EXCLUDED.hierarchy_path, \
                embedding_fingerprint = EXCLUDED.embedding_fingerprint;",
        )
        .map_err(StorageError::PostgresClient)?;
    let edge = client
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
    Ok(LegiProjectionStatements {
        document,
        chunk,
        edge,
    })
}

/// Upsert `documents` (and their chunks/edges) using pre-prepared statements. Prefer this in a batch
/// loop so the statements are parsed once via [`prepare_legi_projection_statements`].
pub fn insert_legi_documents_with_statements<C: GenericClient>(
    client: &mut C,
    statements: &LegiProjectionStatements,
    documents: &[CanonicalDocument],
    chunk_embedding_fingerprint: Option<&str>,
) -> Result<CanonicalInsertReport, StorageError> {
    // Cheap Arc-backed clones so the existing `&statement` execute calls below are unchanged.
    let document_statement = statements.document.clone();
    let chunk_statement = statements.chunk.clone();
    let edge_statement = statements.edge.clone();

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
        let document_hierarchy_path = serde_json::to_string(&document.hierarchy_path)?;
        client
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
                    &document_hierarchy_path,
                    &canonical_json,
                ],
            )
            .map_err(StorageError::PostgresClient)?;

        for chunk in &document.chunks {
            let source_fields = serde_json::to_string(&chunk.source_fields)?;
            let hierarchy_path = serde_json::to_string(&chunk.hierarchy_path)?;
            let chunk_index =
                i32::try_from(chunk.chunk_index).map_err(|_| StorageError::Projection {
                    message: format!(
                        "chunk `{}` has chunk_index too large for storage: {}",
                        chunk.chunk_id, chunk.chunk_index
                    ),
                })?;
            client
                .execute(
                    &chunk_statement,
                    &[
                        &chunk.chunk_id,
                        &chunk.document_id,
                        &chunk_index,
                        &chunk.body,
                        &chunk.contextualized_body,
                        &chunk.chunk_kind,
                        &chunk.chunking,
                        &chunk.boundary,
                        &source_fields,
                        &chunk.source_payload_hash,
                        &chunk.chunk_builder_version,
                        &hierarchy_path,
                        &chunk_embedding_fingerprint,
                    ],
                )
                .map_err(StorageError::PostgresClient)?;
            chunks += 1;
        }

        for edge in &document.publisher_edges {
            insert_graph_edge(client, &edge_statement, edge)?;
            publisher_edges += 1;
        }
    }

    Ok(CanonicalInsertReport {
        documents: documents.len(),
        chunks,
        publisher_edges,
        inferred_edges: 0,
    })
}

/// Convenience wrapper: prepare the projection statements then upsert `documents`. For a single
/// call (or tests); batch callers should prepare once and use [`insert_legi_documents_with_statements`].
pub fn insert_legi_documents_with_client<C: GenericClient>(
    client: &mut C,
    documents: &[CanonicalDocument],
    chunk_embedding_fingerprint: Option<&str>,
) -> Result<CanonicalInsertReport, StorageError> {
    let statements = prepare_legi_projection_statements(client)?;
    insert_legi_documents_with_statements(
        client,
        &statements,
        documents,
        chunk_embedding_fingerprint,
    )
}
