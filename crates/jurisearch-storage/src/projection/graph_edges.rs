//! Graph-edge insertion.

use super::*;

/// Insert one canonical graph edge (publisher or inferred). The stored `edge_source` column is taken
/// from `edge.edge_source`, so publisher and inferred edges remain distinguishable in queries.
pub(super) fn insert_graph_edge(
    client: &mut impl GenericClient,
    statement: &postgres::Statement,
    edge: &CanonicalGraphEdge,
) -> Result<(), StorageError> {
    let payload = serde_json::to_string(edge)?;
    client
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
