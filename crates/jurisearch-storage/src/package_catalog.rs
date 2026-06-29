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
    /// The frozen outbox window this package covers (plan P4 D1): `(low, high]`. A baseline is `(0, high]`.
    pub included_change_seq_low: i64,
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
                  included_change_seq_low, included_change_seq_high, previous_package_id, \
                  previous_package_digest, package_digest, manifest_digest, schema_version, \
                  embedding_fingerprint, builder_versions, status) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15::text::jsonb,$16) \
             ON CONFLICT (package_id) DO NOTHING;",
            &[
                &row.corpus,
                &row.package_sequence,
                &row.package_id,
                &row.package_kind,
                &row.baseline_id,
                &row.generation,
                &row.included_change_seq_low,
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
                    included_change_seq_low, included_change_seq_high, package_digest, \
                    manifest_digest, schema_version, embedding_fingerprint \
             FROM package_catalog WHERE package_id = $1;",
            &[&row.package_id],
        )
        .map_err(StorageError::PostgresClient)?;
    let matches = existing.get::<_, String>("corpus") == row.corpus
        && existing.get::<_, i64>("package_sequence") == row.package_sequence
        && existing.get::<_, String>("package_kind") == row.package_kind
        && existing.get::<_, String>("baseline_id") == row.baseline_id
        && existing.get::<_, String>("generation") == row.generation
        && existing.get::<_, i64>("included_change_seq_low") == row.included_change_seq_low
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
    let mut client = postgres.client()?;
    insert_package_catalog_row(&mut client, row)
}

/// The newest catalog row for a corpus — the chain link + window seed for the next incremental
/// (plan P4 D2): the next package's `lo = included_change_seq_high`,
/// `expected_client_from_sequence = package_sequence`, and `previous_package_*` come from here.
#[derive(Debug, Clone, PartialEq)]
pub struct LatestPackage {
    pub package_sequence: i64,
    pub package_id: String,
    pub package_digest: Option<String>,
    pub baseline_id: String,
    pub generation: String,
    pub included_change_seq_high: i64,
    /// Content-compatibility stamps (plan P4 BLOCKER): the next ordinary incremental MUST match these,
    /// or it has crossed a boundary that needs a re-baseline, not an incremental.
    pub schema_version: i32,
    pub embedding_fingerprint: String,
    pub builder_versions: serde_json::Value,
}

/// Read the newest (highest `package_sequence`) catalog row for `corpus`, or `None` if none exists.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn latest_package_for_corpus<C: GenericClient>(
    client: &mut C,
    corpus: &str,
) -> Result<Option<LatestPackage>, StorageError> {
    let row = client
        .query_opt(
            "SELECT package_sequence, package_id, package_digest, baseline_id, generation, \
                    included_change_seq_high, schema_version, embedding_fingerprint, builder_versions \
             FROM package_catalog WHERE corpus = $1 \
             ORDER BY package_sequence DESC LIMIT 1;",
            &[&corpus],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(row.map(|row| LatestPackage {
        package_sequence: row.get("package_sequence"),
        package_id: row.get("package_id"),
        package_digest: row.get("package_digest"),
        baseline_id: row.get("baseline_id"),
        generation: row.get("generation"),
        included_change_seq_high: row.get("included_change_seq_high"),
        schema_version: row.get("schema_version"),
        embedding_fingerprint: row.get("embedding_fingerprint"),
        builder_versions: row.get("builder_versions"),
    }))
}

/// The status of the catalog row for `(corpus, package_id)`, or `None` if no such row exists. Used by
/// the resume path to VERIFY a staged artifact is cataloged BEFORE publishing it (M3 r3 / Codex BLOCKER
/// 2): an UNCATALOGED staged manifest is an incomplete build (the crash hit before the catalog insert) to
/// DISCARD, never a phantom package to publish + adopt.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn package_catalog_status<C: GenericClient>(
    client: &mut C,
    corpus: &str,
    package_id: &str,
) -> Result<Option<String>, StorageError> {
    let row = client
        .query_opt(
            "SELECT status FROM package_catalog WHERE corpus = $1 AND package_id = $2;",
            &[&corpus, &package_id],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(row.map(|row| row.get("status")))
}

/// Delete an UNPUBLISHED (`'built'`) catalog row for `(corpus, package_id)` — the rebaseline
/// discard-and-rebuild step (M3 r3). A crashed rebaseline attempt's orphaned `'built'` row must never be
/// selectable as a chain head, conflict a fresh re-insert of the same id, or surface in the served
/// manifest. The `status = 'built'` guard means a `'published'` row is NEVER deleted, so a served package
/// can never be removed. Returns the number of rows deleted (0 when the staged artifact was uncataloged).
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn delete_unpublished_package_row<C: GenericClient>(
    client: &mut C,
    corpus: &str,
    package_id: &str,
) -> Result<u64, StorageError> {
    client
        .execute(
            "DELETE FROM package_catalog WHERE corpus = $1 AND package_id = $2 AND status = 'built';",
            &[&corpus, &package_id],
        )
        .map_err(StorageError::PostgresClient)
}

/// A catalog row read back for the producer remote-manifest builder / `package list` (plan P9).
#[derive(Debug, Clone)]
pub struct CatalogRow {
    pub package_sequence: i64,
    pub package_id: String,
    pub package_kind: String,
    pub baseline_id: String,
    pub generation: String,
    pub included_change_seq_low: i64,
    pub included_change_seq_high: i64,
    pub package_digest: Option<String>,
    /// The canonical EMBEDDED-manifest digest (distinct from the payload `package_digest`) — the
    /// remote-manifest builder recomputes it to bind the published embedded manifest's full identity
    /// (not just the payload bytes) to the catalog (plan P9 r1 BLOCKER).
    pub manifest_digest: Option<String>,
    pub schema_version: i32,
    pub embedding_fingerprint: String,
    pub builder_versions: serde_json::Value,
    pub status: String,
}

/// Every catalog row for `corpus`, ordered by `package_sequence` (the retained chain the remote
/// manifest is built over — NOT just the newest, plan P9).
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn catalog_rows_for_corpus<C: GenericClient>(
    client: &mut C,
    corpus: &str,
) -> Result<Vec<CatalogRow>, StorageError> {
    let rows = client
        .query(
            "SELECT package_sequence, package_id, package_kind, baseline_id, generation, \
                    included_change_seq_low, included_change_seq_high, package_digest, manifest_digest, \
                    schema_version, embedding_fingerprint, builder_versions, status \
             FROM package_catalog WHERE corpus = $1 ORDER BY package_sequence;",
            &[&corpus],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(rows
        .into_iter()
        .map(|row| CatalogRow {
            package_sequence: row.get("package_sequence"),
            package_id: row.get("package_id"),
            package_kind: row.get("package_kind"),
            baseline_id: row.get("baseline_id"),
            generation: row.get("generation"),
            included_change_seq_low: row.get("included_change_seq_low"),
            included_change_seq_high: row.get("included_change_seq_high"),
            package_digest: row.get("package_digest"),
            manifest_digest: row.get("manifest_digest"),
            schema_version: row.get("schema_version"),
            embedding_fingerprint: row.get("embedding_fingerprint"),
            builder_versions: row.get("builder_versions"),
            status: row.get("status"),
        })
        .collect())
}

/// The newest MEDIA root (`baseline`/`rebaseline`) for `corpus` — the active baseline the remote
/// manifest advertises and the supersession point for the retained incremental chain (plan P9 Q5).
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn latest_media_package_for_corpus<C: GenericClient>(
    client: &mut C,
    corpus: &str,
) -> Result<Option<CatalogRow>, StorageError> {
    Ok(catalog_rows_for_corpus(client, corpus)?
        .into_iter()
        .rfind(|row| row.package_kind == "baseline" || row.package_kind == "rebaseline"))
}

/// The base of the per-corpus package-BUILD advisory lock (plan P4 D1: serialize concurrent builds of
/// the SAME corpus so two builders never read the same previous catalog row and race the next
/// `package_sequence`). Used as `pg_advisory_lock(BASE, hashtext(corpus))`; cross-corpus builds are
/// independent (different second key).
pub const PACKAGE_BUILD_LOCK_BASE: i32 = 0x6a72_6231; // "jrb1"

/// Acquire the per-corpus package-build lock (SESSION-scoped; the caller releases it). Held across a
/// corpus's "read latest catalog → build → write next catalog" so the per-corpus chain is serial.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn acquire_corpus_build_lock(
    client: &mut postgres::Client,
    corpus: &str,
) -> Result<(), StorageError> {
    client
        .execute(
            "SELECT pg_advisory_lock($1, hashtext($2));",
            &[&PACKAGE_BUILD_LOCK_BASE, &corpus],
        )
        .map(|_| ())
        .map_err(StorageError::PostgresClient)
}

/// Release the per-corpus package-build lock taken by [`acquire_corpus_build_lock`].
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn release_corpus_build_lock(
    client: &mut postgres::Client,
    corpus: &str,
) -> Result<(), StorageError> {
    client
        .execute(
            "SELECT pg_advisory_unlock($1, hashtext($2));",
            &[&PACKAGE_BUILD_LOCK_BASE, &corpus],
        )
        .map(|_| ())
        .map_err(StorageError::PostgresClient)
}
