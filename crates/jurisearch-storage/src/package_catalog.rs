//! Producer package catalog writer (migration v21, plan P3 D5; design §5.1 "two sequence layers").
//!
//! The catalog is the PRODUCER-side bridge between the global `change_seq` (the outbox ordering) and
//! the per-corpus `package_sequence` (the manifest/cursor ordering): each row freezes the global
//! `change_seq` high-water mark a package was built from, so the next incremental has a well-defined
//! `lo` to diff from and a cross-corpus gap is never a false `sequence_gap`. Producer-only — the client
//! never reads it. The writer is here (storage owns the table) so the producer crate stays I/O-thin.

use crate::runtime::{ManagedPostgres, StorageError};
use postgres::GenericClient;

/// One producer-catalog row (the columns of `public.package_catalog`). `serde_json::Value` is used for
/// `builder_versions` (a JSON object) to avoid a serde dependency leak across the storage boundary.
#[derive(Debug, Clone)]
pub struct PackageCatalogRow<'a> {
    pub corpus: &'a str,
    pub package_sequence: i64,
    pub package_id: &'a str,
    pub package_kind: &'a str,
    pub baseline_id: &'a str,
    pub generation: &'a str,
    pub included_change_seq_high: i64,
    pub previous_package_id: Option<&'a str>,
    pub previous_package_digest: Option<&'a str>,
    pub package_digest: Option<&'a str>,
    pub manifest_digest: Option<&'a str>,
    pub schema_version: i32,
    pub embedding_fingerprint: &'a str,
    pub builder_versions: &'a serde_json::Value,
    pub status: &'a str,
}

/// Insert a catalog row for a freshly built package. Idempotency is **identity-checked** (plan P3
/// WARN-3): a re-insert of the SAME `package_id` is a no-op only when every immutable field matches; if
/// a re-build changed the artifact (different digest / window / stamps) the existing row is NOT silently
/// kept — a [`StorageError::PackageCatalog`] is raised so stale baseline metadata cannot mask a changed
/// package. The `(corpus, package_sequence)` PK and the `package_id` unique index guard the table.
///
/// # Errors
/// [`StorageError::PackageCatalog`] on an identity conflict; [`StorageError::PostgresClient`] on a DB
/// error.
pub fn insert_package_catalog_row<C: GenericClient>(
    client: &mut C,
    row: &PackageCatalogRow<'_>,
) -> Result<(), StorageError> {
    let builder_versions = serde_json::to_string(row.builder_versions)?;
    let inserted = client
        .execute(
            "INSERT INTO package_catalog \
                 (corpus, package_sequence, package_id, package_kind, baseline_id, generation, \
                  included_change_seq_high, previous_package_id, previous_package_digest, \
                  package_digest, manifest_digest, schema_version, embedding_fingerprint, \
                  builder_versions, status) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14::text::jsonb,$15) \
             ON CONFLICT (package_id) DO NOTHING;",
            &[
                &row.corpus,
                &row.package_sequence,
                &row.package_id,
                &row.package_kind,
                &row.baseline_id,
                &row.generation,
                &row.included_change_seq_high,
                &row.previous_package_id,
                &row.previous_package_digest,
                &row.package_digest,
                &row.manifest_digest,
                &row.schema_version,
                &row.embedding_fingerprint,
                &builder_versions,
                &row.status,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    if inserted == 1 {
        return Ok(());
    }

    // Conflict on `package_id`: accept ONLY if every immutable identity field matches.
    let existing = client
        .query_one(
            "SELECT corpus, package_sequence, package_kind, baseline_id, generation, \
                    included_change_seq_high, package_digest, manifest_digest, schema_version, \
                    embedding_fingerprint \
             FROM package_catalog WHERE package_id = $1;",
            &[&row.package_id],
        )
        .map_err(StorageError::PostgresClient)?;
    let matches = existing.get::<_, String>("corpus") == row.corpus
        && existing.get::<_, i64>("package_sequence") == row.package_sequence
        && existing.get::<_, String>("package_kind") == row.package_kind
        && existing.get::<_, String>("baseline_id") == row.baseline_id
        && existing.get::<_, String>("generation") == row.generation
        && existing.get::<_, i64>("included_change_seq_high") == row.included_change_seq_high
        && existing
            .get::<_, Option<String>>("package_digest")
            .as_deref()
            == row.package_digest
        && existing
            .get::<_, Option<String>>("manifest_digest")
            .as_deref()
            == row.manifest_digest
        && existing.get::<_, i32>("schema_version") == row.schema_version
        && existing.get::<_, String>("embedding_fingerprint") == row.embedding_fingerprint;
    if matches {
        Ok(())
    } else {
        Err(StorageError::PackageCatalog {
            message: format!(
                "package_id `{}` already cataloged with DIFFERENT immutable fields (a changed re-build)",
                row.package_id
            ),
        })
    }
}

/// Convenience wrapper that opens a connection and inserts one catalog row.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn write_package_catalog_row(
    postgres: &ManagedPostgres,
    row: &PackageCatalogRow<'_>,
) -> Result<(), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    insert_package_catalog_row(&mut client, row)
}
