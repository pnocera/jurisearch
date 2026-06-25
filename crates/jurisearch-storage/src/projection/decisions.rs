//! Jurisprudence decision document projection (+ shared statement prep alias).

use super::*;

/// The document/chunk/edge projection statements are not LEGI-specific: the `documents`, `chunks`,
/// and `graph_edges` columns are identical for `kind='article'` and `kind='decision'`. This alias
/// documents that the jurisprudence decision projection reuses the same prepared statements.
pub type DocumentProjectionStatements = LegiProjectionStatements;

/// Prepare the shared document/chunk/edge projection statements (see [`DocumentProjectionStatements`]).
pub fn prepare_document_projection_statements<C: GenericClient>(
    client: &mut C,
) -> Result<DocumentProjectionStatements, StorageError> {
    prepare_legi_projection_statements(client)
}

/// Project canonical jurisprudence decisions into `documents` (`kind='decision'`), their heuristic
/// chunks, and publisher graph edges. Mirrors the LEGI projection but uses decision-specific
/// validation and temporal mapping: `valid_from = decision_date` (an indexing convenience, not legal
/// validity) and `valid_to = NULL` because decisions are dated, not versioned.
pub fn insert_decision_documents(
    postgres: &ManagedPostgres,
    decisions: &[CanonicalDecision],
    chunk_embedding_fingerprint: Option<&str>,
) -> Result<CanonicalInsertReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    let report = insert_decision_documents_with_client(
        &mut transaction,
        decisions,
        chunk_embedding_fingerprint,
    )?;
    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(report)
}

/// Convenience wrapper: prepare the projection statements then upsert `decisions`. Batch callers
/// should prepare once and use [`insert_decision_documents_with_statements`].
pub fn insert_decision_documents_with_client<C: GenericClient>(
    client: &mut C,
    decisions: &[CanonicalDecision],
    chunk_embedding_fingerprint: Option<&str>,
) -> Result<CanonicalInsertReport, StorageError> {
    let statements = prepare_document_projection_statements(client)?;
    insert_decision_documents_with_statements(
        client,
        &statements,
        decisions,
        chunk_embedding_fingerprint,
    )
}

/// Upsert decisions (and their chunks/edges) using pre-prepared statements.
pub fn insert_decision_documents_with_statements<C: GenericClient>(
    client: &mut C,
    statements: &DocumentProjectionStatements,
    decisions: &[CanonicalDecision],
    chunk_embedding_fingerprint: Option<&str>,
) -> Result<CanonicalInsertReport, StorageError> {
    let document_statement = statements.document.clone();
    let chunk_statement = statements.chunk.clone();
    let edge_statement = statements.edge.clone();

    let mut chunks = 0usize;
    let mut publisher_edges = 0usize;
    let mut inferred_edges = 0usize;
    // Decisions are dated, not versioned: valid_from indexes on decision_date, valid_to/raw are null.
    let valid_to: Option<String> = None;
    let valid_to_raw: Option<String> = None;
    let version_group: Option<String> = None;
    let empty_hierarchy_path = "[]".to_owned();

    for decision in decisions {
        decision
            .validate()
            .map_err(|error| StorageError::Projection {
                message: format!(
                    "canonical decision `{}` failed validation before storage projection: {error}",
                    decision.document_id
                ),
            })?;
        let canonical_json = serde_json::to_string(decision)?;
        client
            .execute(
                &document_statement,
                &[
                    &decision.document_id,
                    &decision.source,
                    &decision.kind,
                    &decision.source_uid,
                    &version_group,
                    &decision.citation,
                    &decision.title,
                    &decision.body,
                    &decision.decision_date,
                    &valid_to,
                    &valid_to_raw,
                    &decision.source_url,
                    &decision.source_payload_hash,
                    &empty_hierarchy_path,
                    &canonical_json,
                ],
            )
            .map_err(StorageError::PostgresClient)?;

        for chunk in &decision.chunks {
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

        for edge in &decision.publisher_edges {
            insert_graph_edge(client, &edge_statement, edge)?;
            publisher_edges += 1;
        }
        for edge in &decision.inferred_edges {
            insert_graph_edge(client, &edge_statement, edge)?;
            inferred_edges += 1;
        }
    }

    Ok(CanonicalInsertReport {
        documents: decisions.len(),
        chunks,
        publisher_edges,
        inferred_edges,
    })
}
