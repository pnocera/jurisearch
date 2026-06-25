//! Replay-snapshot report + component caching/refresh.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaySnapshotMode {
    Cached,
    Refresh,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaySnapshotReport {
    pub documents: ReplaySnapshotComponent,
    pub chunks: ReplaySnapshotComponent,
    pub publisher_edges: ReplaySnapshotComponent,
    pub embeddings: ReplaySnapshotComponent,
    pub manifests: ReplaySnapshotComponent,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaySnapshotComponent {
    pub count: i64,
    pub signature: String,
}

impl ReplaySnapshotReport {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            documents: ReplaySnapshotComponent::empty(),
            chunks: ReplaySnapshotComponent::empty(),
            publisher_edges: ReplaySnapshotComponent::empty(),
            embeddings: ReplaySnapshotComponent::empty(),
            manifests: ReplaySnapshotComponent::empty(),
            signature: String::new(),
        }
    }

    #[must_use]
    pub fn status(&self) -> &'static str {
        if self.documents.count == 0
            && self.chunks.count == 0
            && self.publisher_edges.count == 0
            && self.embeddings.count == 0
        {
            "empty"
        } else {
            "available"
        }
    }
}

impl ReplaySnapshotComponent {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            count: 0,
            signature: String::new(),
        }
    }
}

pub fn refresh_replay_snapshot(
    postgres: &ManagedPostgres,
) -> Result<ReplaySnapshotReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    refresh_replay_snapshot_with_client(&mut client)
}

pub fn refresh_replay_snapshot_with_client(
    client: &mut postgres::Client,
) -> Result<ReplaySnapshotReport, StorageError> {
    let snapshot = load_replay_snapshot(client)?;
    store_replay_snapshot(client, &snapshot)?;
    Ok(snapshot)
}

fn load_replay_snapshot(
    client: &mut postgres::Client,
) -> Result<ReplaySnapshotReport, StorageError> {
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    transaction
        .batch_execute("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ;")
        .map_err(StorageError::PostgresClient)?;
    let documents = snapshot_component(
        &mut transaction,
        "documents",
        "SELECT document_id AS row_key, \
                md5(concat_ws(chr(31), document_id, source, kind, source_uid, \
                    coalesce(version_group, ''), coalesce(citation, ''), \
                    coalesce(title, ''), body, coalesce(valid_from::text, ''), \
                    coalesce(valid_to::text, ''), coalesce(valid_to_raw, ''), \
                    coalesce(source_url, ''), source_payload_hash, hierarchy_path::text, \
                    canonical_json::text)) AS row_hash \
         FROM documents",
    )?;
    let chunks = snapshot_component(
        &mut transaction,
        "chunks",
        "SELECT chunk_id AS row_key, \
                md5(concat_ws(chr(31), chunk_id, document_id, chunk_index::text, body, \
                    chunk_kind, source_fields::text, source_payload_hash, \
                    chunk_builder_version, coalesce(embedding_fingerprint, ''))) AS row_hash \
         FROM chunks",
    )?;
    let publisher_edges = snapshot_component(
        &mut transaction,
        "publisher_edges",
        "SELECT edge_id AS row_key, \
                md5(concat_ws(chr(31), edge_id, coalesce(from_document_id, ''), \
                    coalesce(to_document_id, ''), edge_kind, edge_source, payload::text)) AS row_hash \
         FROM graph_edges \
         WHERE edge_source = 'publisher'",
    )?;
    let embeddings = snapshot_component(
        &mut transaction,
        "chunk_embeddings",
        "SELECT chunk_id AS row_key, \
                md5(concat_ws(chr(31), chunk_id, embedding_fingerprint, embedding::text, \
                    model, dimension::text)) AS row_hash \
         FROM chunk_embeddings",
    )?;
    let manifests = snapshot_component(
        &mut transaction,
        "index_manifest",
        "SELECT key AS row_key, \
                md5(concat_ws(chr(31), key, value::text)) AS row_hash \
         FROM index_manifest \
         WHERE key NOT IN ('replay_snapshot', 'query_readiness')",
    )?;
    let signature_input = format!(
        "documents:{}:{}|chunks:{}:{}|publisher_edges:{}:{}|embeddings:{}:{}|manifests:{}:{}",
        documents.count,
        documents.signature,
        chunks.count,
        chunks.signature,
        publisher_edges.count,
        publisher_edges.signature,
        embeddings.count,
        embeddings.signature,
        manifests.count,
        manifests.signature
    );
    let signature = transaction
        .query_one("SELECT md5($1);", &[&signature_input])
        .map_err(StorageError::PostgresClient)?
        .get(0);
    transaction.commit().map_err(StorageError::PostgresClient)?;

    Ok(ReplaySnapshotReport {
        documents,
        chunks,
        publisher_edges,
        embeddings,
        manifests,
        signature,
    })
}

pub(super) fn load_cached_replay_snapshot<C: GenericClient>(
    client: &mut C,
) -> Result<Option<ReplaySnapshotReport>, StorageError> {
    let Some(row) = client
        .query_opt(
            "SELECT value::text \
             FROM index_manifest \
             WHERE key = 'replay_snapshot';",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
    else {
        return Ok(None);
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&row.get::<_, String>(0)) else {
        return Ok(None);
    };
    if let Some(snapshot) = value.get("snapshot") {
        Ok(serde_json::from_value(snapshot.clone()).ok())
    } else {
        Ok(serde_json::from_value(value).ok())
    }
}

fn store_replay_snapshot<C: GenericClient>(
    client: &mut C,
    snapshot: &ReplaySnapshotReport,
) -> Result<(), StorageError> {
    let manifest = serde_json::json!({
        "schema_version": "1",
        "snapshot": snapshot,
    })
    .to_string();
    client
        .execute(
            "INSERT INTO index_manifest(key, value, updated_at) \
             VALUES ('replay_snapshot', $1::text::jsonb, now()) \
             ON CONFLICT (key) DO UPDATE \
             SET value = EXCLUDED.value, \
                 updated_at = EXCLUDED.updated_at;",
            &[&manifest],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}

fn snapshot_component<C: GenericClient>(
    client: &mut C,
    component_name: &str,
    rows_sql: &str,
) -> Result<ReplaySnapshotComponent, StorageError> {
    let sql = format!(
        "SELECT count(*)::bigint, \
                md5(coalesce(string_agg(row_hash, E'\\n' ORDER BY row_key), '')) \
         FROM ({rows_sql}) {component_name}_snapshot_rows;"
    );
    let row = client
        .query_one(sql.as_str(), &[])
        .map_err(StorageError::PostgresClient)?;
    Ok(ReplaySnapshotComponent {
        count: row.get(0),
        signature: row.get(1),
    })
}
