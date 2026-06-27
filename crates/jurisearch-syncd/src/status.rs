//! `corpus status`: report the cursor authority's view of each installed corpus (plan P3 C2, full
//! compat stamps in P10). Reads `jurisearch_control.corpus_state` (the single source of position truth)
//! — never the generations directly, so the report can never disagree with what readers see. `Serialize`
//! so the management CLI can emit stable JSON (`status --json`); diagnostics still go to stderr.

use jurisearch_storage::backend::WriterConnection;
use jurisearch_storage::runtime::StorageError;
use serde::Serialize;

/// One installed corpus's full cursor position + compatibility stamps (P10 observability — the same
/// stamps the planner reads, so `status` and catch-up never disagree).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CorpusStatus {
    pub corpus: String,
    pub active_generation: String,
    pub sequence: i64,
    pub baseline_id: String,
    pub schema_version: i32,
    pub embedding_fingerprint: String,
    pub builder_versions: serde_json::Value,
    pub last_package_id: Option<String>,
    pub last_package_digest: Option<String>,
    pub applied_at: Option<String>,
}

/// Report every installed corpus, ordered by name. An empty result means no corpus is installed yet
/// (a fresh client).
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn corpus_status(client: &dyn WriterConnection) -> Result<Vec<CorpusStatus>, StorageError> {
    let mut db = client.writer_client()?;
    let rows = db
        .query(
            "SELECT corpus, active_generation, sequence, baseline_id, schema_version, \
                    embedding_fingerprint, builder_versions, last_package_id, last_package_digest, \
                    applied_at::text AS applied_at \
             FROM jurisearch_control.corpus_state ORDER BY corpus;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(rows
        .iter()
        .map(|row| CorpusStatus {
            corpus: row.get("corpus"),
            active_generation: row.get("active_generation"),
            sequence: row.get("sequence"),
            baseline_id: row.get("baseline_id"),
            schema_version: row.get("schema_version"),
            embedding_fingerprint: row.get("embedding_fingerprint"),
            builder_versions: row.get("builder_versions"),
            last_package_id: row.get("last_package_id"),
            last_package_digest: row.get("last_package_digest"),
            applied_at: row.get("applied_at"),
        })
        .collect())
}
