//! `corpus status`: report the cursor authority's view of each installed corpus (plan P3 C2). Reads
//! `jurisearch_control.corpus_state` (the single source of position truth) — never the generations
//! directly, so the report can never disagree with what readers see.

use jurisearch_storage::runtime::{ManagedPostgres, StorageError};

/// One installed corpus's position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusStatus {
    pub corpus: String,
    pub active_generation: String,
    pub sequence: i64,
    pub baseline_id: String,
    pub schema_version: i32,
    pub last_package_id: Option<String>,
}

/// Report every installed corpus, ordered by name. An empty result means no corpus is installed yet
/// (a fresh client).
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn corpus_status(client: &ManagedPostgres) -> Result<Vec<CorpusStatus>, StorageError> {
    let mut db = client.client()?;
    let rows = db
        .query(
            "SELECT corpus, active_generation, sequence, baseline_id, schema_version, last_package_id \
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
            last_package_id: row.get("last_package_id"),
        })
        .collect())
}
