//! Baseline applier (plan P3, C3-baseline + C4): verify → load into a fresh generation → build indexes
//! → validate postconditions → atomic activation. The corpus is **never** advertised query-ready until
//! its indexes are built (INV-6): all work happens inside an invisible `building` generation, and the
//! only globally-visible mutation is the atomic [`activate_generation`] switch.

use std::io::Write;
use std::path::Path;

use jurisearch_package::compat::Version;
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::manifest::embedded::PayloadFormat;
use jurisearch_package::signed::Signed;
use jurisearch_package::{PackageKind, RejectCode, Verifier};

/// The version of THIS client binary, gated against each package's `minimum_client_version` (§10).
pub const CLIENT_VERSION: Version = Version::new(0, 1, 0);

use jurisearch_storage::dense::{DENSE_VECTOR_INDEX_NAME, recommended_probes};
use jurisearch_storage::generations::{
    ActivationStamps, CursorGuard, DenseManifestEntry, GenerationIndexReport, REPLICATED_TABLES,
    acquire_corpus_apply_lock, activate_generation_with_guard, build_generation_indexes,
    create_generation_load_tables, generation_counter_of, generation_name, generation_schema,
    release_corpus_apply_lock, reset_building_generation, schema_for_generation,
};
use jurisearch_storage::generations::{primary_key_columns, replicated_table_columns};
use jurisearch_storage::incremental::{
    advance_corpus_cursor, apply_deletes, apply_replace_set, apply_upserts, has_cascade_fks,
    replace_set_rows,
};
use jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION;
use jurisearch_storage::outbox::{
    DigestSource, TableDigest, corpus_table_digests, corpus_table_digests_with_client,
};
use jurisearch_storage::runtime::{ManagedPostgres, sql_identifier};
use jurisearch_storage::zone_units::ZONE_UNIT_VECTOR_INDEX_NAME;

use jurisearch_package::event::{ReplaceSet, ReplaceSetGroup, set_digest_over_rows};

use crate::error::SyncError;

/// The outcome of applying a baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaselineApplyOutcome {
    /// The baseline was applied and the corpus is now live at `sequence` on `generation`.
    Applied {
        corpus: String,
        generation: String,
        sequence: u64,
        index_report: String,
    },
    /// The package was already applied (cursor at the result sequence with matching identity) — a
    /// no-op re-apply (INV-3 idempotency).
    AlreadyApplied { corpus: String, sequence: u64 },
}

/// Apply a baseline artifact directory onto `client` (plan P3) — a thin wrapper over the shared media
/// apply with the first-baseline cursor guard. `verifier` checks the manifest signature.
///
/// # Errors
/// [`SyncError`] with a [`RejectCode`] on a contract refusal, or a storage/IO error.
pub fn apply_baseline(
    client: &ManagedPostgres,
    artifact_dir: &Path,
    verifier: &dyn Verifier,
) -> Result<BaselineApplyOutcome, SyncError> {
    apply_media_package(client, artifact_dir, verifier, MediaApplyMode::Baseline)
}

/// Apply a RE-BASELINE artifact directory onto `client` (plan P5): a full reissue that scope-replaces
/// ONLY this corpus's server set — loaded into a fresh generation off the live read path, then swapped
/// in by the §7.4 short switch with a FORWARD-SUPERSESSION cursor guard, so a long-offline client (or
/// one partway through the superseded incremental chain) jumps forward to the re-baseline's sequence.
/// `jurisearch_app` and every other installed corpus are untouched (INV-4/5).
///
/// # Errors
/// [`SyncError`] with a [`RejectCode`] on a contract refusal, or a storage/IO error.
pub fn apply_rebaseline(
    client: &ManagedPostgres,
    artifact_dir: &Path,
    verifier: &dyn Verifier,
) -> Result<BaselineApplyOutcome, SyncError> {
    apply_media_package(client, artifact_dir, verifier, MediaApplyMode::Rebaseline)
}

/// Which media package is being applied — selects the accepted `package_kind`, whether a client BEHIND
/// the package may apply it, and the §7.3 cursor guard at the switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaApplyMode {
    Baseline,
    Rebaseline,
}

impl MediaApplyMode {
    fn expected_kind(self) -> PackageKind {
        match self {
            MediaApplyMode::Baseline => PackageKind::Baseline,
            MediaApplyMode::Rebaseline => PackageKind::Rebaseline,
        }
    }

    /// A re-baseline is self-sufficient: applying it onto a corpus BEHIND its result sequence is valid
    /// (the point of the P7 baseline shortcut). A first baseline onto an installed corpus is not.
    fn behind_is_applicable(self) -> bool {
        matches!(self, MediaApplyMode::Rebaseline)
    }
}

/// Shared media (baseline / re-baseline) apply: verify → gates → per-file digests → idempotency →
/// load into a fresh `building` generation NAMED BY THE MANIFEST → build indexes → validate index
/// contract + postconditions → atomic switch (cursor guard + dense `index_manifest`, one txn). The
/// corpus is never query-ready until its indexes are built (INV-6); the only globally-visible mutation
/// is the [`activate_generation_with_guard`] switch.
fn apply_media_package(
    client: &ManagedPostgres,
    artifact_dir: &Path,
    verifier: &dyn Verifier,
    mode: MediaApplyMode,
) -> Result<BaselineApplyOutcome, SyncError> {
    // 1. Read + verify the signed manifest.
    let manifest_bytes = std::fs::read(jurisearch_package::artifact::manifest_path(artifact_dir))?;
    let signed: Signed<EmbeddedManifest> = serde_json::from_slice(&manifest_bytes)?;
    signed
        .verify(verifier)
        .map_err(|error| SyncError::reject(RejectCode::SignatureInvalid, error.to_string()))?;
    let manifest = &signed.payload;
    let corpus = manifest.identity.corpus.as_str().to_owned();

    if manifest.identity.package_kind != mode.expected_kind() {
        return Err(SyncError::reject(
            RejectCode::BaselineRequired,
            format!(
                "apply expected a `{}` package, got `{}`",
                mode.expected_kind().as_str(),
                manifest.identity.package_kind.as_str()
            ),
        ));
    }

    // 2. Compatibility gates (§10): client version, schema version + bundle digest, required
    //    extensions, and the CopyBinary loopback guard — the consumer enforces the SIGNED contract,
    //    not just the schema number.
    check_client_version(manifest)?;
    check_schema_compatibility(client, manifest)?;
    check_extensions(client, manifest)?;
    check_copy_binary_guard(client, manifest)?;

    // 2b. Entitlement precondition (§11.3, plan P6): a non-open package needs a valid installed license
    //     token covering its corpus/tier — checked BEFORE any payload work.
    crate::trust::check_entitlement(client, manifest)?;

    // 3. Per-file digests (§11.1): every payload file (and the aggregate `artifact_sha256`) must match
    //    its declared digest — checked from bytes read off disk, AFTER the embedded signature above
    //    (plan P6 r1: no redundant "internal canonical digest" step; the signature already covers the
    //    canonical manifest bytes, so a separate `canonical_digest(manifest)` proved nothing).
    verify_per_file_digests(artifact_dir, manifest)?;

    // 4. Serialize the whole apply for this corpus under the per-corpus apply lock (plan P5 r1
    //    BLOCKER): no concurrent apply can build the same deterministic generation, and the retriable
    //    building-generation reset can never drop a generation another apply is actively loading. Held
    //    across idempotency → reset → create → load → build → switch; released on EVERY path.
    let mut db = client.client()?;
    acquire_corpus_apply_lock(&mut db, &corpus)?;
    let outcome = apply_media_locked(client, &mut db, artifact_dir, manifest, mode);
    let _ = release_corpus_apply_lock(&mut db, &corpus);
    outcome
}

/// The locked body of [`apply_media_package`], run while holding the per-corpus apply lock on `db`:
/// idempotency (RE-CHECKED under the lock, since a concurrent apply may have advanced the cursor) →
/// load into the manifest-named `building` generation → build + validate → atomic switch.
fn apply_media_locked(
    client: &ManagedPostgres,
    db: &mut postgres::Client,
    artifact_dir: &Path,
    manifest: &EmbeddedManifest,
    mode: MediaApplyMode,
) -> Result<BaselineApplyOutcome, SyncError> {
    let corpus = manifest.identity.corpus.as_str().to_owned();
    let result_sequence = manifest.apply.result_sequence.get();
    let package_id = manifest.identity.package_id.as_str();
    // The cursor identity is the PACKAGE/artifact digest — the SAME value the producer catalog stores
    // as `package_digest`, so the P4 chain link compares like-for-like (plan P3 r2 WARN-1).
    let package_digest = manifest.integrity.artifact_sha256.as_str();

    // Idempotency (D7), RE-CHECKED under the apply lock: both the package id AND digest are compared. A
    // re-baseline is self-sufficient, so a client BEHIND its result sequence proceeds (not rejected as
    // `BaselineRequired`).
    if let Some(outcome) = idempotency_decision(
        client,
        &corpus,
        result_sequence,
        package_id,
        package_digest,
        mode.behind_is_applicable(),
    )? {
        return Ok(outcome);
    }

    // Load into a fresh `building` generation NAMED BY THE MANIFEST (plan P5 critical gap): the physical
    // generation a client creates is the producer's DETERMINISTIC label, so a later incremental's
    // `active_generation` precondition resolves even on a fresh client that jumped straight to a
    // re-baseline. A leftover `building` generation from a failed prior attempt at the same label is
    // reset first (safe: the apply lock proves no concurrent apply owns it), so the apply is retriable.
    let generation = manifest.identity.generation.clone();
    let counter = generation_counter_of(&corpus, &generation).ok_or_else(|| {
        SyncError::reject(
            RejectCode::DigestMismatch,
            format!("manifest generation `{generation}` is not a <corpus>_g<NNNN> label"),
        )
    })?;
    let schema = generation_schema(&corpus, counter);
    debug_assert_eq!(schema, schema_for_generation(&generation));

    reset_building_generation(db, &corpus, &generation)?;
    let created =
        create_generation_load_tables(db, &corpus, counter, Some(&manifest.identity.baseline_id))?;
    debug_assert_eq!(created, generation);

    copy_payload_in(db, artifact_dir, manifest, &schema)?;

    // Build indexes INSIDE the generation, before it can ever be read (§9.3, INV-6).
    let index_report = build_generation_indexes(db, &generation, &corpus)?;

    // Enforce the producer's index contract: every declared BM25/IVFFlat index must exist in the
    // generation with the right access method/table/column/lists/probes BEFORE the switch.
    validate_index_contract(db, &schema, manifest)?;

    // Assemble the dense `index_manifest` rows (with the package-declared `default_probes`). They are
    // written INSIDE the activation transaction (plan P5 isolation fix), not before — so a re-baseline
    // never mutates global dense query metadata while the OLD generation is still live.
    let dense_manifests = dense_manifest_entries(&schema, manifest);

    // Validate postconditions against the producer's declared digests BEFORE the switch.
    validate_postconditions(client, &corpus, &schema, manifest)?;

    // Atomic activation: the single globally-visible mutation, with the mode's cursor guard and the
    // dense `index_manifest` write folded into the same short transaction.
    let builder_versions = builder_versions_json(manifest);
    let stamps = ActivationStamps {
        sequence: i64::try_from(result_sequence).unwrap_or(i64::MAX),
        baseline_id: &manifest.identity.baseline_id,
        schema_version: manifest.compatibility.schema_version,
        embedding_fingerprint: &manifest.compatibility.embedding_fingerprint,
        builder_versions: &builder_versions,
        last_package_id: Some(package_id),
        last_package_digest: Some(package_digest),
    };
    let guard = match mode {
        // A first baseline expects no prior cursor (idempotency already rejected an existing one with a
        // different identity) — the §7.3 "first baseline" guard.
        MediaApplyMode::Baseline => CursorGuard::FirstBaseline,
        // A re-baseline supersedes forward: any cursor strictly behind `result_sequence` jumps to it.
        MediaApplyMode::Rebaseline => CursorGuard::RebaselineForward {
            result_sequence: i64::try_from(result_sequence).unwrap_or(i64::MAX),
        },
    };
    activate_generation_with_guard(
        client,
        &corpus,
        &generation,
        &stamps,
        guard,
        &dense_manifests,
    )?;

    Ok(BaselineApplyOutcome::Applied {
        corpus,
        generation,
        sequence: result_sequence,
        index_report: format_index_report(&index_report),
    })
}

/// The client DB must already be at the package's `schema_version` (P3 ships no migration bundle).
fn check_schema_compatibility(
    client: &ManagedPostgres,
    manifest: &EmbeddedManifest,
) -> Result<(), SyncError> {
    let applied = client
        .execute_sql("SELECT coalesce(max(version), 0)::text FROM schema_migrations;")?
        .trim()
        .parse::<i32>()
        .unwrap_or(0);
    if applied > CURRENT_SCHEMA_VERSION {
        return Err(SyncError::reject(
            RejectCode::SchemaAhead,
            format!(
                "client DB schema {applied} is ahead of this binary ({CURRENT_SCHEMA_VERSION})"
            ),
        ));
    }
    if manifest.compatibility.schema_version != applied {
        return Err(SyncError::reject(
            RejectCode::SchemaAhead,
            format!(
                "package schema_version {} != client DB schema {applied}; migrate the client first",
                manifest.compatibility.schema_version
            ),
        ));
    }
    // Beyond the schema NUMBER, the client's migration set must match the producer's (plan P3 WARN-1):
    // recompute the bundle digest with the SAME helper the producer used and compare.
    let mut db = client.client()?;
    let bundle = jurisearch_storage::migrations::schema_bundle_digest(&mut db)?;
    if bundle != manifest.compatibility.schema_migration_bundle_digest {
        return Err(SyncError::reject(
            RejectCode::SchemaAhead,
            format!(
                "client schema bundle digest {bundle} != package {}; the client's migration set differs",
                manifest.compatibility.schema_migration_bundle_digest
            ),
        ));
    }
    Ok(())
}

/// Enforce the package's `minimum_client_version` against this binary (§10).
fn check_client_version(manifest: &EmbeddedManifest) -> Result<(), SyncError> {
    if !CLIENT_VERSION.satisfies_minimum(manifest.compatibility.minimum_client_version) {
        return Err(SyncError::reject(
            RejectCode::ClientTooOld,
            format!(
                "client {CLIENT_VERSION} is older than the package minimum {}",
                manifest.compatibility.minimum_client_version
            ),
        ));
    }
    Ok(())
}

/// Every `requires_extensions` entry must be installed on the client (plan P3 WARN-1, §6.2.2).
fn check_extensions(
    client: &ManagedPostgres,
    manifest: &EmbeddedManifest,
) -> Result<(), SyncError> {
    let mut db = client.client()?;
    for ext in &manifest.compatibility.requires_extensions {
        let present = db
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM pg_extension WHERE extname = $1);",
                &[&ext.name],
            )
            .map_err(SyncError::Postgres)?
            .get::<_, bool>(0);
        if !present {
            return Err(SyncError::reject(
                RejectCode::ExtensionMissing,
                format!(
                    "required extension `{}` is not installed on the client",
                    ext.name
                ),
            ));
        }
    }
    Ok(())
}

/// After the index build, prove the generation satisfies the producer's SIGNED index contract — not
/// just that an index of the declared name exists, but its access method, target table, indexed
/// column, and (for IVFFlat) the `lists` reloption (plan P3 r2 WARN-2). A drifted index with the right
/// name but wrong shape can never activate. Validated against `pg_catalog`, so even a consumer build
/// bug is caught.
fn validate_index_contract(
    db: &mut postgres::Client,
    schema: &str,
    manifest: &EmbeddedManifest,
) -> Result<(), SyncError> {
    for name in &manifest.apply.index_build.bm25_indexes {
        let shape = index_shape(db, schema, name)?.ok_or_else(|| {
            SyncError::reject(
                RejectCode::DigestMismatch,
                format!("declared bm25 index `{name}` missing from generation `{schema}`"),
            )
        })?;
        if shape.access_method != "bm25" {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!("index `{name}` is `{}`, expected bm25", shape.access_method),
            ));
        }
        if !REPLICATED_TABLES.contains(&shape.table.as_str()) {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!("bm25 index `{name}` is on non-replicated `{}`", shape.table),
            ));
        }
    }
    for ivf in &manifest.apply.index_build.ivfflat_finalize {
        let shape = index_shape(db, schema, &ivf.index)?.ok_or_else(|| {
            SyncError::reject(
                RejectCode::DigestMismatch,
                format!(
                    "declared ivfflat index `{}` missing from generation `{schema}`",
                    ivf.index
                ),
            )
        })?;
        if shape.access_method != "ivfflat" {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!(
                    "index `{}` is `{}`, expected ivfflat",
                    ivf.index, shape.access_method
                ),
            ));
        }
        if !REPLICATED_TABLES.contains(&shape.table.as_str()) {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!(
                    "ivfflat index `{}` on non-replicated `{}`",
                    ivf.index, shape.table
                ),
            ));
        }
        if shape.indexed_column.as_deref() != Some("embedding") {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!(
                    "ivfflat index `{}` is on column {:?}, expected `embedding`",
                    ivf.index, shape.indexed_column
                ),
            ));
        }
        if shape.lists.as_deref() != Some(ivf.lists.to_string().as_str()) {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!(
                    "ivfflat index `{}` has lists={:?}, contract declared {}",
                    ivf.index, shape.lists, ivf.lists
                ),
            ));
        }
        // The signed `probes` must be internally consistent with `lists` (plan P3 r3 WARN-2) — a
        // tampered probe value (it tunes recall on the client) is rejected.
        let expected_probes = recommended_probes(ivf.lists);
        if ivf.probes != expected_probes {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!(
                    "ivfflat index `{}` declares probes={}, expected {expected_probes} for lists={}",
                    ivf.index, ivf.probes, ivf.lists
                ),
            ));
        }
    }
    Ok(())
}

/// Assemble the dense `index_manifest` rows declared by the manifest (the package-tuned `default_probes`
/// for each IVFFlat index over its loaded generation, plan P3 r3 WARN-2). Returned for the caller to
/// write INSIDE the activation transaction (plan P5), rather than mutating global dense metadata before
/// the switch. Maps each declared IVFFlat index to its `index_manifest` key + parent/embedding tables.
fn dense_manifest_entries(schema: &str, manifest: &EmbeddedManifest) -> Vec<DenseManifestEntry> {
    let compat = &manifest.compatibility;
    let mut entries = Vec::new();
    for ivf in &manifest.apply.index_build.ivfflat_finalize {
        let (key, parent_table, embedding_table) = if ivf.index == DENSE_VECTOR_INDEX_NAME {
            ("embedding", "chunks", "chunk_embeddings")
        } else if ivf.index == ZONE_UNIT_VECTOR_INDEX_NAME {
            ("zone_embedding", "zone_units", "zone_unit_embeddings")
        } else {
            continue; // an unknown dense index is not part of the P3 manifest contract
        };
        entries.push(DenseManifestEntry {
            schema: schema.to_owned(),
            key: key.to_owned(),
            parent_table: parent_table.to_owned(),
            embedding_table: embedding_table.to_owned(),
            index_name: ivf.index.clone(),
            lists: ivf.lists,
            default_probes: ivf.probes,
            embedding_fingerprint: compat.embedding_fingerprint.clone(),
            embedding_model: compat.embedding_model.clone(),
            embedding_dimension: i32::try_from(compat.embedding_dimension).unwrap_or(0),
            embedding_normalize: compat.embedding_normalize,
        });
    }
    entries
}

/// The shape of a generation index for contract validation: access method, target table, the first
/// indexed column (read structurally from `pg_attribute`/`indkey`, not string-matched), and the
/// `lists` reloption (IVFFlat).
struct IndexShape {
    access_method: String,
    table: String,
    indexed_column: Option<String>,
    lists: Option<String>,
}

fn index_shape(
    db: &mut postgres::Client,
    schema: &str,
    name: &str,
) -> Result<Option<IndexShape>, SyncError> {
    let row = db
        .query_opt(
            "SELECT a.amname AS access_method, t.relname AS table_name, \
                    (SELECT att.attname FROM pg_attribute att \
                     WHERE att.attrelid = x.indrelid AND att.attnum = x.indkey[0]) AS indexed_column, \
                    (SELECT split_part(opt, '=', 2) FROM unnest(i.reloptions) opt \
                     WHERE opt LIKE 'lists=%') AS lists \
             FROM pg_index x \
             JOIN pg_class i ON i.oid = x.indexrelid \
             JOIN pg_class t ON t.oid = x.indrelid \
             JOIN pg_am a ON a.oid = i.relam \
             JOIN pg_namespace n ON n.oid = i.relnamespace \
             WHERE n.nspname = $1 AND i.relname = $2;",
            &[&schema, &name],
        )
        .map_err(SyncError::Postgres)?;
    Ok(row.map(|row| IndexShape {
        access_method: row.get("access_method"),
        table: row.get("table_name"),
        indexed_column: row.get("indexed_column"),
        lists: row.get("lists"),
    }))
}

/// `COPY (FORMAT binary)` is tied to the server's type layout, so a binary baseline must declare a
/// `postgres_major` window and the consumer must fall inside it (plan P3 D2).
fn check_copy_binary_guard(
    client: &ManagedPostgres,
    manifest: &EmbeddedManifest,
) -> Result<(), SyncError> {
    let uses_binary = manifest
        .payload
        .files
        .iter()
        .any(|file| file.format == PayloadFormat::CopyBinary);
    if !uses_binary {
        return Ok(());
    }
    let (Some(min), Some(max)) = (
        manifest.compatibility.postgres_major_min,
        manifest.compatibility.postgres_major_max,
    ) else {
        return Err(SyncError::reject(
            RejectCode::ExtensionMissing,
            "binary COPY payload must declare postgres_major_min/max (loopback guard)",
        ));
    };
    let major = client.server_version_major()?;
    if major < min || major > max {
        return Err(SyncError::reject(
            RejectCode::ExtensionMissing,
            format!("client PostgreSQL major {major} outside the package window [{min},{max}]"),
        ));
    }
    Ok(())
}

/// Read every payload file and check its digest against the manifest before any load (§11.1).
fn verify_per_file_digests(
    artifact_dir: &Path,
    manifest: &EmbeddedManifest,
) -> Result<(), SyncError> {
    // Build a `verified` map from the bytes ACTUALLY read off disk (plan P3 r4 WARN-1) — never trusting
    // the signed `integrity.per_file_digests` map for the aggregate.
    let mut verified: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for file in &manifest.payload.files {
        let name = &file.file_name;
        let path = jurisearch_package::artifact::payload_file_path(artifact_dir, name);
        let bytes = std::fs::read(&path)?;
        let digest = jurisearch_package::canonical::digest_bytes(&bytes);
        if digest != file.digest {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!(
                    "payload `{name}` digest {digest} != declared {}",
                    file.digest
                ),
            ));
        }
        if verified.insert(name.clone(), digest).is_some() {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!("duplicate payload file `{name}` in payload.files"),
            ));
        }
    }
    // The verified set (built from real bytes) must EXACTLY equal the signed `integrity.per_file_digests`
    // — no missing, extra, or differing entries — so the aggregate can only be computed over proven bytes.
    if verified != manifest.integrity.per_file_digests {
        return Err(SyncError::reject(
            RejectCode::DigestMismatch,
            "integrity.per_file_digests does not exactly match the verified payload files"
                .to_owned(),
        ));
    }
    // Recompute the AGGREGATE package digest over the VERIFIED set with the SAME shared definition the
    // producer used, and require it to equal BOTH integrity digests (the cursor identity is derived from
    // `artifact_sha256`, so this binds the cursor strictly to the applied payload bytes).
    let aggregate = jurisearch_package::artifact::aggregate_payload_digest(&verified);
    if aggregate != manifest.integrity.artifact_sha256 {
        return Err(SyncError::reject(
            RejectCode::DigestMismatch,
            format!(
                "aggregate payload digest {aggregate} != integrity.artifact_sha256 {}",
                manifest.integrity.artifact_sha256
            ),
        ));
    }
    if aggregate != manifest.integrity.uncompressed_payload_digest {
        return Err(SyncError::reject(
            RejectCode::DigestMismatch,
            format!(
                "aggregate payload digest {aggregate} != integrity.uncompressed_payload_digest {}",
                manifest.integrity.uncompressed_payload_digest
            ),
        ));
    }
    Ok(())
}

/// Decide idempotency from `corpus_state` (D7). Returns `Some(no-op outcome)` to skip, `None` to apply.
/// A no-op requires BOTH the same `package_id` AND the same package digest at the result sequence (plan
/// P3 r2 WARN-1) — so distinct packages with identical payload bytes are not confused.
fn idempotency_decision(
    client: &ManagedPostgres,
    corpus: &str,
    result_sequence: u64,
    package_id: &str,
    package_digest: &str,
    behind_is_applicable: bool,
) -> Result<Option<BaselineApplyOutcome>, SyncError> {
    let mut db = client.client()?;
    let row = db
        .query_opt(
            "SELECT sequence, last_package_id, last_package_digest \
             FROM jurisearch_control.corpus_state WHERE corpus = $1;",
            &[&corpus],
        )
        .map_err(SyncError::Postgres)?;
    let Some(row) = row else {
        return Ok(None); // fresh corpus: apply
    };
    let current: i64 = row.get("sequence");
    let current = u64::try_from(current).unwrap_or(0);
    let current_id: Option<String> = row.get("last_package_id");
    let current_digest: Option<String> = row.get("last_package_digest");
    if current == result_sequence {
        if current_id.as_deref() == Some(package_id)
            && current_digest.as_deref() == Some(package_digest)
        {
            return Ok(Some(BaselineApplyOutcome::AlreadyApplied {
                corpus: corpus.to_owned(),
                sequence: current,
            }));
        }
        return Err(SyncError::reject(
            RejectCode::DigestMismatch,
            format!(
                "corpus `{corpus}` already at sequence {current} with a DIFFERENT package id/digest; refusing"
            ),
        ));
    }
    if current > result_sequence {
        return Err(SyncError::reject(
            RejectCode::WrongGeneration,
            format!(
                "corpus `{corpus}` is at sequence {current}, ahead of this package's {result_sequence}"
            ),
        ));
    }
    // current < result_sequence with an existing cursor. A self-sufficient re-baseline (full reload)
    // legitimately supersedes an installed-but-behind corpus → proceed. A first baseline onto an
    // installed corpus is wrong → BaselineRequired (a re-baseline is the right package, P5).
    if behind_is_applicable {
        return Ok(None);
    }
    Err(SyncError::reject(
        RejectCode::BaselineRequired,
        format!("corpus `{corpus}` already installed at {current}; a re-baseline is P5, not P3"),
    ))
}

/// COPY each payload file into the generation, in the manifest's dependency `apply_order` (§5.2/§6.2.2).
fn copy_payload_in(
    db: &mut postgres::Client,
    artifact_dir: &Path,
    manifest: &EmbeddedManifest,
    schema: &str,
) -> Result<(), SyncError> {
    if !manifest.payload.citation_order_holds() {
        return Err(SyncError::reject(
            RejectCode::DigestMismatch,
            "payload apply_order violates the official_api_responses-before-citations rule",
        ));
    }
    for table in &manifest.payload.apply_order {
        let Some(file) = manifest.payload.files.iter().find(|f| &f.table == table) else {
            continue; // a table can be absent from a payload (no rows) but present in apply_order
        };
        let path = jurisearch_package::artifact::payload_file_path(artifact_dir, &file.file_name);
        let bytes = std::fs::read(&path)?;
        let columns = file
            .columns
            .iter()
            .map(|col| sql_identifier(col))
            .collect::<Vec<_>>()
            .join(", ");
        let format = match file.format {
            PayloadFormat::CopyBinary => "binary",
            PayloadFormat::Jsonl | PayloadFormat::Parquet => {
                return Err(SyncError::reject(
                    RejectCode::ExtensionMissing,
                    format!("P3 applies only CopyBinary payloads, got {:?}", file.format),
                ));
            }
        };
        let copy_sql = format!(
            "COPY {}.{} ({columns}) FROM STDIN (FORMAT {format})",
            sql_identifier(schema),
            sql_identifier(&file.table),
        );
        let mut writer = db.copy_in(copy_sql.as_str()).map_err(SyncError::Postgres)?;
        writer.write_all(&bytes)?;
        writer.finish().map_err(SyncError::Postgres)?;
    }
    Ok(())
}

/// Recompute the generation's per-table digests with the SAME code path the producer used, and require
/// an exact match to the manifest postconditions before the cursor advances (§11.1, D6).
fn validate_postconditions(
    client: &ManagedPostgres,
    corpus: &str,
    schema: &str,
    manifest: &EmbeddedManifest,
) -> Result<(), SyncError> {
    let digests = corpus_table_digests(client, corpus, DigestSource::Generation { schema })?;
    let post = &manifest.apply.postconditions;
    for TableDigest {
        table_name,
        row_count,
        digest,
    } in &digests
    {
        let expected_count = post.row_counts.get(table_name).copied().unwrap_or(0);
        if *row_count != expected_count {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!("postcondition row_count[{table_name}] {row_count} != {expected_count}"),
            ));
        }
        let expected_digest = post.table_digests.get(table_name);
        if expected_digest != Some(digest) {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!("postcondition table_digest[{table_name}] {digest} != {expected_digest:?}"),
            ));
        }
    }
    Ok(())
}

fn builder_versions_json(manifest: &EmbeddedManifest) -> serde_json::Value {
    serde_json::Value::Object(
        manifest
            .compatibility
            .builder_versions
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    )
}

fn format_index_report(report: &GenerationIndexReport) -> String {
    format!(
        "constraints={} indexes={} ivfflat={:?}",
        report.constraints_built, report.indexes_built, report.ivfflat_built
    )
}

/// Used only by `status` for the generation-name preview (kept here so the apply path owns naming).
#[must_use]
pub fn baseline_generation_name(corpus: &str, counter: u32) -> String {
    generation_name(corpus, counter)
}

/// The outcome of applying an incremental.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncrementalApplyOutcome {
    /// Applied and the corpus cursor advanced to `sequence`; `scopes` semantic scopes were applied.
    Applied {
        corpus: String,
        sequence: u64,
        scopes: usize,
    },
    /// Already at the result sequence with matching identity — an idempotent no-op (INV-3).
    AlreadyApplied { corpus: String, sequence: u64 },
}

/// Apply an incremental artifact directory onto the corpus's ACTIVE generation in ONE cursor-gated
/// transaction (plan P4 D4, §7.3): verify → gates → cursor/chain check → apply the JSONL diff with
/// row-level index maintenance → validate the whole-corpus postconditions in-txn → advance the cursor.
///
/// # Errors
/// [`SyncError`] with a [`RejectCode`] on a contract refusal (incl. `SequenceGap`), or a storage/IO error.
pub fn apply_incremental(
    client: &ManagedPostgres,
    artifact_dir: &Path,
    verifier: &dyn Verifier,
) -> Result<IncrementalApplyOutcome, SyncError> {
    let manifest_bytes = std::fs::read(jurisearch_package::artifact::manifest_path(artifact_dir))?;
    let signed: Signed<EmbeddedManifest> = serde_json::from_slice(&manifest_bytes)?;
    signed
        .verify(verifier)
        .map_err(|error| SyncError::reject(RejectCode::SignatureInvalid, error.to_string()))?;
    let manifest = &signed.payload;
    let corpus = manifest.identity.corpus.as_str().to_owned();
    if manifest.identity.package_kind != PackageKind::Incremental {
        return Err(SyncError::reject(
            RejectCode::BaselineRequired,
            format!(
                "apply_incremental only applies `incremental` packages, got `{}`",
                manifest.identity.package_kind.as_str()
            ),
        ));
    }

    // Gates (no CopyBinary guard — JSONL is portable) + entitlement (§11.3, plan P6) + payload digests.
    check_client_version(manifest)?;
    check_schema_compatibility(client, manifest)?;
    check_extensions(client, manifest)?;
    crate::trust::check_entitlement(client, manifest)?;
    verify_per_file_digests(artifact_dir, manifest)?;

    let package_id = manifest.identity.package_id.as_str();
    let package_digest = manifest.integrity.artifact_sha256.as_str();
    let result_sequence = manifest.apply.result_sequence.get();
    let expected_from = manifest.apply.expected_client_from_sequence.get();

    // Precompute the non-generated column list + PK for every replicated table (the schema is stable).
    let mut meta = client.client()?;
    let mut columns_map: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    let mut pk_map: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for table in REPLICATED_TABLES {
        columns_map.insert(
            (*table).to_owned(),
            replicated_table_columns(&mut meta, table)?,
        );
        pk_map.insert((*table).to_owned(), primary_key_columns(&mut meta, table)?);
    }

    // ONE transaction on the active generation.
    let mut db = client.client()?;
    let mut tx = db.transaction().map_err(SyncError::Postgres)?;
    tx.batch_execute("SET LOCAL lock_timeout = '5s';")
        .map_err(SyncError::Postgres)?;
    let locked: bool = tx
        .query_one(
            "SELECT pg_try_advisory_xact_lock($1);",
            &[&jurisearch_storage::generations::APPLY_ADVISORY_LOCK_KEY],
        )
        .map_err(SyncError::Postgres)?
        .get(0);
    if !locked {
        return Err(SyncError::reject(
            RejectCode::WrongGeneration,
            "another apply holds the advisory lock".to_owned(),
        ));
    }

    let row = tx
        .query_opt(
            "SELECT sequence, last_package_id, last_package_digest, active_generation, baseline_id, \
                    schema_version, embedding_fingerprint, builder_versions \
             FROM jurisearch_control.corpus_state WHERE corpus = $1 FOR UPDATE;",
            &[&corpus],
        )
        .map_err(SyncError::Postgres)?;
    let Some(row) = row else {
        return Err(SyncError::reject(
            RejectCode::BaselineRequired,
            format!("corpus `{corpus}` is not installed; apply a baseline first"),
        ));
    };
    let current = u64::try_from(row.get::<_, i64>("sequence")).unwrap_or(0);
    let cur_id: Option<String> = row.get("last_package_id");
    let cur_digest: Option<String> = row.get("last_package_digest");
    let active_generation: String = row.get("active_generation");
    let baseline_id: String = row.get("baseline_id");
    let cur_schema: i32 = row.get("schema_version");
    let cur_fingerprint: String = row.get("embedding_fingerprint");
    let cur_builders: serde_json::Value = row.get("builder_versions");

    // Idempotency + ordering (D6).
    if current == result_sequence {
        if cur_id.as_deref() == Some(package_id) && cur_digest.as_deref() == Some(package_digest) {
            tx.commit().map_err(SyncError::Postgres)?;
            return Ok(IncrementalApplyOutcome::AlreadyApplied {
                corpus,
                sequence: current,
            });
        }
        return Err(SyncError::reject(
            RejectCode::DigestMismatch,
            format!("corpus `{corpus}` already at {current} with a different package; refusing"),
        ));
    }
    if current > result_sequence {
        return Err(SyncError::reject(
            RejectCode::WrongGeneration,
            format!("corpus `{corpus}` is at {current}, ahead of this package's {result_sequence}"),
        ));
    }
    if current != expected_from {
        return Err(SyncError::reject(
            RejectCode::SequenceGap,
            format!(
                "corpus `{corpus}` is at {current}, package expects from-sequence {expected_from}"
            ),
        ));
    }
    // Chain link + active-state preconditions.
    if manifest.identity.previous_package_id.as_deref() != cur_id.as_deref() {
        return Err(SyncError::reject(
            RejectCode::WrongGeneration,
            "previous_package_id does not match the corpus cursor".to_owned(),
        ));
    }
    if manifest.identity.previous_package_sha256.as_deref() != cur_digest.as_deref() {
        return Err(SyncError::reject(
            RejectCode::DigestMismatch,
            "previous_package_sha256 does not match the corpus cursor".to_owned(),
        ));
    }
    if let Some(expected) = &manifest.apply.preconditions.active_baseline_id
        && *expected != baseline_id
    {
        return Err(SyncError::reject(
            RejectCode::WrongGeneration,
            "active_baseline_id precondition mismatch".to_owned(),
        ));
    }
    if let Some(expected) = &manifest.apply.preconditions.active_generation
        && *expected != active_generation
    {
        return Err(SyncError::reject(
            RejectCode::WrongGeneration,
            "active_generation precondition mismatch".to_owned(),
        ));
    }

    // Content-compatibility preconditions (plan P4 BLOCKER): the cursor's stamps MUST match the signed
    // preconditions before any row is touched — an ordinary incremental that crossed a fingerprint /
    // builder / schema boundary is rejected (it needs a re-baseline), even if its row digests happen to
    // line up.
    let pre = &manifest.apply.preconditions;
    if pre.schema_version != cur_schema {
        return Err(SyncError::reject(
            RejectCode::SchemaAhead,
            format!(
                "precondition schema_version {} != cursor {cur_schema}",
                pre.schema_version
            ),
        ));
    }
    if pre.embedding_fingerprint != cur_fingerprint {
        return Err(SyncError::reject(
            RejectCode::EmbeddingFingerprintMismatch,
            format!(
                "precondition embedding_fingerprint `{}` != cursor `{cur_fingerprint}`",
                pre.embedding_fingerprint
            ),
        ));
    }
    let pre_builders =
        serde_json::to_value(&pre.builder_versions).unwrap_or(serde_json::Value::Null);
    if pre_builders != cur_builders {
        return Err(SyncError::reject(
            RejectCode::BuilderVersionMismatch,
            "precondition builder_versions != cursor builder_versions".to_owned(),
        ));
    }

    let schema = schema_for_generation(&active_generation);
    if !has_cascade_fks(&mut tx, &schema)? {
        return Err(SyncError::reject(
            RejectCode::WrongGeneration,
            format!("generation `{schema}` is missing the cascade FKs replace-sets rely on"),
        ));
    }

    let scopes = apply_incremental_files(
        &mut tx,
        &schema,
        artifact_dir,
        manifest,
        &columns_map,
        &pk_map,
    )?;

    // Postconditions IN-TXN (so they see the uncommitted apply) — the convergence proof.
    let digests = corpus_table_digests_with_client(
        &mut tx,
        &corpus,
        DigestSource::Generation { schema: &schema },
    )?;
    let post = &manifest.apply.postconditions;
    for TableDigest {
        table_name,
        row_count,
        digest,
    } in &digests
    {
        if post.row_counts.get(table_name).copied().unwrap_or(0) != *row_count {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!("postcondition row_count[{table_name}] mismatch after incremental apply"),
            ));
        }
        if post.table_digests.get(table_name) != Some(digest) {
            return Err(SyncError::reject(
                RejectCode::DigestMismatch,
                format!(
                    "postcondition table_digest[{table_name}] mismatch after incremental apply"
                ),
            ));
        }
    }

    advance_corpus_cursor(
        &mut tx,
        &corpus,
        i64::try_from(result_sequence).unwrap_or(i64::MAX),
        package_id,
        package_digest,
    )?;
    tx.commit().map_err(SyncError::Postgres)?;

    Ok(IncrementalApplyOutcome::Applied {
        corpus,
        sequence: result_sequence,
        scopes,
    })
}

/// Apply the diff's JSONL files in the manifest's dependency order onto `schema`. Returns the scope
/// count. Replace-set digests are verified against the generation rows post-apply (§5.3).
fn apply_incremental_files(
    tx: &mut postgres::Transaction<'_>,
    schema: &str,
    artifact_dir: &Path,
    manifest: &EmbeddedManifest,
    columns_map: &std::collections::BTreeMap<String, Vec<String>>,
    pk_map: &std::collections::BTreeMap<String, Vec<String>>,
) -> Result<usize, SyncError> {
    let mut scopes = 0usize;
    for token in &manifest.payload.apply_order {
        for file in manifest.payload.files.iter().filter(|f| &f.table == token) {
            let path =
                jurisearch_package::artifact::payload_file_path(artifact_dir, &file.file_name);
            let text = std::fs::read_to_string(&path)?;
            match file.op {
                jurisearch_package::EventKind::Upsert => {
                    let rows = parse_jsonl(&text)?;
                    let pk = pk_map.get(token).cloned().unwrap_or_default();
                    apply_upserts(tx, schema, token, &file.columns, &pk, &rows)?;
                    scopes += rows.len();
                }
                jurisearch_package::EventKind::Delete => {
                    let keys = parse_jsonl(&text)?;
                    let pk = pk_map.get(token).cloned().unwrap_or_default();
                    apply_deletes(tx, schema, token, &pk, &keys)?;
                    scopes += keys.len();
                }
                jurisearch_package::EventKind::ReplaceSet => {
                    let group = replace_set_group(token).ok_or_else(|| {
                        SyncError::reject(
                            RejectCode::DigestMismatch,
                            format!("unknown replace_set group token `{token}`"),
                        )
                    })?;
                    for line in text.lines().filter(|l| !l.trim().is_empty()) {
                        let rs: ReplaceSet = serde_json::from_str(line)?;
                        let doc = rs.scope.document_id.clone();
                        apply_replace_set(tx, schema, group, &doc, &rs.rows, |table| {
                            Ok(columns_map.get(table).cloned().unwrap_or_default())
                        })?;
                        let actual = replace_set_rows(tx, schema, group, &doc)?;
                        if set_digest_over_rows(&actual) != rs.set_digest {
                            return Err(SyncError::reject(
                                RejectCode::DigestMismatch,
                                format!("replace_set set_digest mismatch for document `{doc}`"),
                            ));
                        }
                        scopes += 1;
                    }
                }
            }
        }
    }
    Ok(scopes)
}

fn parse_jsonl(text: &str) -> Result<Vec<serde_json::Value>, SyncError> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<serde_json::Value>(l).map_err(SyncError::Json))
        .collect()
}

fn replace_set_group(token: &str) -> Option<ReplaceSetGroup> {
    match token {
        "chunks_with_embeddings" => Some(ReplaceSetGroup::ChunksWithEmbeddings),
        "chunk_embeddings" => Some(ReplaceSetGroup::ChunkEmbeddings),
        "zone_units" => Some(ReplaceSetGroup::ZoneUnits),
        "decision_zones" => Some(ReplaceSetGroup::DecisionZones),
        _ => None,
    }
}
