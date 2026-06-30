//! Baseline builder (plan P3, workstream P3-baseline): materialise a `baseline` artifact for one
//! corpus from the producer's authoritative `public` tables — per-table `COPY (FORMAT binary)` payload
//! files + a signed embedded manifest carrying the apply contract, postcondition digests, and index
//! contract — and seed the producer catalog. Signing is stubbed behind the `Signer` trait (real in P6).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use jurisearch_package::artifact;
use jurisearch_package::canonical::{canonical_digest, digest_bytes, tee_digest};
use jurisearch_package::compat::Version;
use jurisearch_package::corpus::Corpus;
use jurisearch_package::event::EventKind;
use jurisearch_package::manifest::embedded::{
    ApplyContract, Compatibility, Compression, EmbeddedManifest, Entitlement, ExtensionRequirement,
    Identity, IndexBuildContract, Integrity, IvfflatFinalize, OperationCount, PayloadFile,
    PayloadFormat, PayloadLayout, Postconditions, Preconditions, RollbackPolicy,
};
use jurisearch_package::sequence::PackageSequence;
use jurisearch_package::signed::Signed;
use jurisearch_package::{PACKAGE_FORMAT_VERSION, PackageKind, Signer};

use jurisearch_storage::backend::DbClientSource;
use jurisearch_storage::dense::{
    DENSE_VECTOR_INDEX_NAME, recommended_ivfflat_lists, recommended_probes,
};
use jurisearch_storage::generations::{
    REPLICATED_TABLES, baseline_copy_out_select, generation_counter_of, generation_name,
    replicated_table_columns,
};
use jurisearch_storage::outbox::{
    DigestSource, corpus_table_digests_with_client, current_change_seq_with_client,
};
use jurisearch_storage::package_catalog::{
    LatestPackage, PackageCatalogRow, acquire_corpus_build_lock, insert_package_catalog_row,
    latest_package_for_corpus, release_corpus_build_lock,
};
use jurisearch_storage::runtime::StorageError;
use jurisearch_storage::zone_units::ZONE_UNIT_VECTOR_INDEX_NAME;
use postgres::GenericClient;

use crate::error::BuildError;

/// The §5.2/§6.2.2 dependency apply order: base before derived; `official_api_responses` BEFORE the
/// citation tables that reference its `response_id`; embeddings AFTER their parent (chunks/zone units).
/// `PayloadLayout::citation_order_holds()` validates this on the consumer.
const APPLY_ORDER: &[&str] = &[
    "documents",
    "official_api_responses",
    "chunks",
    "chunk_embeddings",
    "graph_edges",
    "legi_metadata_roots",
    "zone_units",
    "zone_unit_embeddings",
    "decision_zones",
    "decision_legislation_citations",
    "legislation_citation_resolutions",
];

/// Inputs that the producer supplies (rather than reading from the DB), kept explicit so a build is
/// deterministic and testable (no ambient clock/embedding guesses).
#[derive(Debug, Clone)]
pub struct BaselineParams {
    /// e.g. `core-2026-06-27-g0001` (design §6.1 baseline identity).
    pub baseline_id: String,
    /// The producer run that built this artifact.
    pub builder_run_id: String,
    /// RFC3339 build timestamp.
    pub created_at: String,
    pub embedding_fingerprint: String,
    pub embedding_model: String,
    pub embedding_dimension: u32,
    pub embedding_normalize: bool,
    pub builder_versions: BTreeMap<String, String>,
    /// The minimum client binary version that can apply this package (§10).
    pub minimum_client_version: Version,
}

/// A summary of what was built (paths + identity), returned to the producer CLI/caller.
#[derive(Debug, Clone)]
pub struct BaselineBuildReport {
    pub corpus: String,
    pub package_id: String,
    pub generation: String,
    pub baseline_id: String,
    pub artifact_dir: PathBuf,
    pub total_rows: u64,
    pub included_change_seq_high: u64,
}

/// A summary of a re-baseline build (plan P5): like [`BaselineBuildReport`] plus the superseding
/// sequence window `from_sequence -> to_sequence` the package advances the corpus through.
#[derive(Debug, Clone)]
pub struct RebaselineBuildReport {
    pub corpus: String,
    pub package_id: String,
    pub generation: String,
    pub baseline_id: String,
    pub from_sequence: u64,
    pub to_sequence: u64,
    pub artifact_dir: PathBuf,
    pub total_rows: u64,
    pub included_change_seq_high: u64,
}

/// The kind-specific identity + sequencing for a full-snapshot media package (a baseline or a
/// re-baseline). Everything else — the fenced one-snapshot COPY-binary payload, the postcondition
/// digests, the signed manifest, the index contract, the catalog seed — is identical between the two
/// kinds and lives in [`build_media_package`] (plan P5: share, don't copy-paste the builder).
struct MediaSpec {
    package_kind: PackageKind,
    /// `identity.from_sequence`.
    from_sequence: PackageSequence,
    /// `identity.to_sequence` == `apply.result_sequence` == catalog `package_sequence`.
    to_sequence: PackageSequence,
    package_id: String,
    generation: String,
    baseline_id: String,
    previous_package_id: Option<String>,
    previous_package_sha256: Option<String>,
    /// catalog `included_change_seq_low` — 0 for a baseline, the prior head's high for a re-baseline.
    included_change_seq_low: i64,
    /// Seed bytes for the (placeholder, until P6) schema-ops digest — kind-distinct.
    schema_ops_label: &'static [u8],
    /// Seed bytes for the (placeholder, until P6) open-entitlement policy digest.
    entitlement_label: &'static [u8],
}

/// What a media build produced (paths + identity), shared by the baseline and re-baseline reports.
#[derive(Debug, Clone)]
struct MediaBuildReport {
    package_id: String,
    generation: String,
    total_rows: u64,
    included_change_seq_high: u64,
}

/// Build a baseline artifact for `corpus` into `artifact_dir` and seed the producer catalog.
///
/// # Errors
/// [`BuildError`] on a DB, IO, canonicalisation, or signing failure.
pub fn build_baseline(
    producer: &impl DbClientSource,
    corpus: &str,
    artifact_dir: &Path,
    signer: &dyn Signer,
    params: &BaselineParams,
) -> Result<BaselineBuildReport, BuildError> {
    // P3 first baseline: per-corpus package sequence starts at 1, generation g0001, no chain link.
    let sequence = PackageSequence::new(1);
    let spec = MediaSpec {
        package_kind: PackageKind::Baseline,
        from_sequence: sequence,
        to_sequence: sequence,
        package_id: format!("{corpus}-{}-{}", sequence.get(), sequence.get()),
        generation: generation_name(corpus, 1),
        baseline_id: params.baseline_id.clone(),
        previous_package_id: None,
        previous_package_sha256: None,
        included_change_seq_low: 0,
        schema_ops_label: b"baseline-no-schema-ops",
        entitlement_label: b"p3-open-entitlement",
    };
    let report = build_media_package(producer, corpus, artifact_dir, signer, params, &spec)?;
    Ok(BaselineBuildReport {
        corpus: corpus.to_owned(),
        package_id: report.package_id,
        generation: report.generation,
        baseline_id: params.baseline_id.clone(),
        artifact_dir: artifact_dir.to_owned(),
        total_rows: report.total_rows,
        included_change_seq_high: report.included_change_seq_high,
    })
}

/// Build a RE-BASELINE artifact for `corpus` (plan P5): a full reissue triggered by a re-embed /
/// builder bump / corpus-rewriting migration whose result already lives in the producer's `public`.
/// Same artifact shape as a baseline but marked `Rebaseline`, on a NEW generation, advancing the
/// per-corpus sequence `N -> N+1` and chain-linking to the prior head. Unlike an incremental it does
/// NOT reject a changed embedding fingerprint / builder versions — superseding them is the point. The
/// per-corpus build lock is held across the catalog read + snapshot so the chain link is coherent.
///
/// # Errors
/// [`BuildError`] if no baseline is cataloged for `corpus`, or on a DB/IO/canonicalisation/signing
/// failure.
pub fn build_rebaseline(
    producer: &impl DbClientSource,
    corpus: &str,
    artifact_dir: &Path,
    signer: &dyn Signer,
    params: &BaselineParams,
) -> Result<RebaselineBuildReport, BuildError> {
    let mut db = producer.client()?;
    acquire_corpus_build_lock(&mut db, corpus)?;
    let built = build_rebaseline_locked(&mut db, producer, corpus, artifact_dir, signer, params);
    let _ = release_corpus_build_lock(&mut db, corpus);
    let (report, from_sequence, to_sequence) = built?;
    Ok(RebaselineBuildReport {
        corpus: corpus.to_owned(),
        package_id: report.package_id,
        generation: report.generation,
        baseline_id: params.baseline_id.clone(),
        from_sequence,
        to_sequence,
        artifact_dir: artifact_dir.to_owned(),
        total_rows: report.total_rows,
        included_change_seq_high: report.included_change_seq_high,
    })
}

/// The re-baseline body, run while holding the per-corpus build lock on `db`: read the prior head,
/// derive the superseding sequence/generation/chain-link `MediaSpec`, and build the media package.
fn build_rebaseline_locked(
    db: &mut postgres::Client,
    producer: &impl DbClientSource,
    corpus: &str,
    artifact_dir: &Path,
    signer: &dyn Signer,
    params: &BaselineParams,
) -> Result<(MediaBuildReport, u64, u64), BuildError> {
    let prev: LatestPackage = latest_package_for_corpus(db, corpus)?.ok_or_else(|| {
        BuildError::Storage(StorageError::Generations {
            message: format!(
                "no baseline cataloged for corpus `{corpus}`; build a baseline before a re-baseline"
            ),
        })
    })?;
    let prev_counter = generation_counter_of(corpus, &prev.generation).ok_or_else(|| {
        BuildError::Storage(StorageError::Generations {
            message: format!(
                "cataloged generation `{}` is not a <corpus>_g<NNNN> label",
                prev.generation
            ),
        })
    })?;
    let from_sequence = PackageSequence::new(u64::try_from(prev.package_sequence).unwrap_or(0));
    let to_sequence = from_sequence.next();
    let spec = MediaSpec {
        package_kind: PackageKind::Rebaseline,
        from_sequence,
        to_sequence,
        package_id: format!("{corpus}-{}-{}", from_sequence.get(), to_sequence.get()),
        // The producer's deterministic generation label the client MUST adopt (plan P5 critical gap):
        // `prev_counter + 1`, so a later incremental's `active_generation` precondition resolves even
        // on a fresh client that jumped straight to this re-baseline.
        generation: generation_name(corpus, prev_counter + 1),
        baseline_id: params.baseline_id.clone(),
        previous_package_id: Some(prev.package_id.clone()),
        previous_package_sha256: prev.package_digest.clone(),
        included_change_seq_low: prev.included_change_seq_high,
        schema_ops_label: b"rebaseline-no-schema-ops",
        entitlement_label: b"p5-open-entitlement",
    };
    let report = build_media_package(producer, corpus, artifact_dir, signer, params, &spec)?;
    Ok((report, from_sequence.get(), to_sequence.get()))
}

/// Shared full-snapshot media builder for [`build_baseline`] and [`build_rebaseline`] (plan P5): cut
/// the ENTIRE package from ONE producer snapshot — a single REPEATABLE READ, read-only transaction
/// backs the digests, per-table COPY reads, the `change_seq` high-water mark, the BM25 inventory, and
/// the schema bundle — then seed the catalog. `spec` carries the only kind-specific identity.
fn build_media_package(
    producer: &impl DbClientSource,
    corpus: &str,
    artifact_dir: &Path,
    signer: &dyn Signer,
    params: &BaselineParams,
    spec: &MediaSpec,
) -> Result<MediaBuildReport, BuildError> {
    let corpus_typed = Corpus::new(corpus.to_owned())?;

    std::fs::create_dir_all(artifact::payload_dir(artifact_dir))?;

    // BLOCKER fix: cut the ENTIRE package from ONE producer snapshot. A single REPEATABLE READ,
    // read-only transaction backs the digests, the per-table COPY reads, the `change_seq` high-water
    // mark, the BM25 inventory, and the schema bundle — so the catalog window can never include a
    // mutation the payload missed (or vice versa) under concurrent ingest. The first query establishes
    // the snapshot; every read below sees exactly that point in time.
    let mut db = producer.client()?;
    // P4 D7 fence (WARN-1: minimal critical section): a DEDICATED connection holds the exclusive outbox
    // fence ONLY while the build snapshot is established + `hi` frozen — so `hi` is a true commit-order
    // high-water mark — then releases it, so concurrent ingest is not blocked through the whole COPY
    // phase. The snapshot transaction (on `db`) stays fixed at `hi` for the rest of the build.
    let mut fence_conn = producer.client()?;
    jurisearch_storage::outbox::acquire_outbox_fence(&mut fence_conn)?;
    let mut tx = db
        .build_transaction()
        .isolation_level(postgres::IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()?;

    // P4 r2 WARN-1: keep the fence's critical section MINIMAL. `current_change_seq_with_client` is a
    // single-row read, so it both establishes the REPEATABLE READ snapshot and freezes `hi` cheaply —
    // do it FIRST, then RELEASE the fence immediately. The expensive full-corpus digest pass (and every
    // COPY/BM25/schema read below) then runs OUTSIDE the fence window on that same fixed snapshot, so a
    // large baseline no longer stalls concurrent ingest commits through the digest phase.
    let change_seq_high = current_change_seq_with_client(&mut tx)?.get();
    jurisearch_storage::outbox::release_outbox_fence(&mut fence_conn)?;
    let digests = corpus_table_digests_with_client(&mut tx, corpus, DigestSource::ProducerPublic)?;
    let mut row_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut table_digests: BTreeMap<String, String> = BTreeMap::new();
    for digest in &digests {
        row_counts.insert(digest.table_name.clone(), digest.row_count);
        table_digests.insert(digest.table_name.clone(), digest.digest.clone());
    }

    // COPY each replicated table out as binary, write the payload file, and record per-file metadata.
    let mut files = Vec::with_capacity(REPLICATED_TABLES.len());
    let mut per_file_digests: BTreeMap<String, String> = BTreeMap::new();
    let mut operations = Vec::with_capacity(REPLICATED_TABLES.len());
    let mut total_rows = 0u64;
    for table in REPLICATED_TABLES {
        let columns = replicated_table_columns(&mut tx, table)?;
        let select = baseline_copy_out_select(&mut tx, table, corpus)?;
        let mut reader =
            tx.copy_out(format!("COPY ({select}) TO STDOUT (FORMAT binary)").as_str())?;

        // Memory fix: stream the COPY-binary output directly to the payload file while hashing it,
        // never materialising the (multi-GB at corpus scale) table in RAM. The file bytes and the
        // per-file digest are byte-identical to the prior `fs::write` + `digest_bytes` path.
        let file_name = artifact::baseline_file_name(table);
        let payload_path = artifact::payload_file_path(artifact_dir, &file_name);
        let mut writer = std::io::BufWriter::new(std::fs::File::create(&payload_path)?);
        let file_digest = tee_digest(&mut reader, &mut writer)?;
        drop(reader);
        // Surface any deferred write error before relying on the digest/file (BufWriter::drop
        // would otherwise swallow it).
        writer.flush()?;
        let row_count = row_counts.get(*table).copied().unwrap_or(0);
        total_rows += row_count;

        per_file_digests.insert(file_name.clone(), file_digest.clone());
        operations.push(OperationCount {
            table: (*table).to_owned(),
            op: EventKind::Upsert,
            count: row_count,
        });
        files.push(PayloadFile {
            file_name,
            table: (*table).to_owned(),
            columns,
            op: EventKind::Upsert,
            format: PayloadFormat::CopyBinary,
            compression: Compression::None,
            row_count,
            digest: file_digest,
        });
    }

    // The aggregate over the per-file `name=digest` pairs — the SHARED definition the consumer recomputes
    // to prove the manifest's aggregate digest matches the payload it actually read. (Real tarball/artifact
    // hashing arrives with transport in a later phase.)
    let payload_digest = artifact::aggregate_payload_digest(&per_file_digests);

    // (`change_seq_high` was frozen under the fence above.) These reads share the same fixed snapshot.
    let bm25_indexes = query_bm25_index_names(&mut tx)?;
    let schema_migration_bundle_digest =
        jurisearch_storage::migrations::schema_bundle_digest(&mut tx)?;
    tx.commit()?;

    // The server major is a static property of the instance (not snapshot-sensitive). Query it through
    // the generic `DbClientSource` seam (a fresh client) so a media build works against either the
    // self-managed `ManagedPostgres` OR an external producer PostgreSQL (`WriterHandle`).
    let server_major = server_version_major(&mut producer.client()?)?;

    // The index contract (observability + the §9.3 directive). The client recomputes `lists` from the
    // loaded rowcount, but the producer declares the same corpus-sized values it would itself use.
    let ivfflat_finalize = vec![
        ivfflat_finalize_for("chunk_embeddings", DENSE_VECTOR_INDEX_NAME, &row_counts),
        ivfflat_finalize_for(
            "zone_unit_embeddings",
            ZONE_UNIT_VECTOR_INDEX_NAME,
            &row_counts,
        ),
    ];

    let manifest = EmbeddedManifest {
        identity: Identity {
            package_format_version: PACKAGE_FORMAT_VERSION,
            package_id: spec.package_id.clone(),
            corpus: corpus_typed.clone(),
            package_kind: spec.package_kind,
            from_sequence: spec.from_sequence,
            to_sequence: spec.to_sequence,
            previous_package_id: spec.previous_package_id.clone(),
            previous_package_sha256: spec.previous_package_sha256.clone(),
            baseline_id: spec.baseline_id.clone(),
            generation: spec.generation.clone(),
            created_at: params.created_at.clone(),
            builder_run_id: params.builder_run_id.clone(),
        },
        compatibility: Compatibility {
            minimum_client_version: params.minimum_client_version,
            maximum_client_version: None,
            schema_version: jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
            schema_migration_bundle_digest,
            requires_extensions: vec![
                ExtensionRequirement {
                    name: "vector".to_owned(),
                    minimum_version: None,
                },
                ExtensionRequirement {
                    name: "pg_search".to_owned(),
                    minimum_version: None,
                },
            ],
            embedding_fingerprint: params.embedding_fingerprint.clone(),
            embedding_model: params.embedding_model.clone(),
            embedding_dimension: params.embedding_dimension,
            embedding_normalize: params.embedding_normalize,
            builder_versions: params.builder_versions.clone(),
            // CopyBinary is loopback-only: pin the producer's PG major so a mismatched consumer rejects.
            postgres_major_min: Some(server_major),
            postgres_major_max: Some(server_major),
        },
        entitlement: Entitlement {
            entitlement_corpus: corpus_typed.clone(),
            tier: "all".to_owned(),
            license_epoch: 0,
            audience: None,
            entitlement_policy_digest: digest_bytes(spec.entitlement_label),
        },
        integrity: Integrity {
            artifact_sha256: payload_digest.clone(),
            uncompressed_payload_digest: payload_digest.clone(),
            per_file_digests,
            canonicalisation_algorithm: "jcs-sha256".to_owned(),
            // The DESCRIPTIVE algorithm field tracks the signer (plan P6) — the authoritative one is
            // `Signed.signature.algorithm`. No more hard-coded "stub" under a real signer.
            signature_algorithm: signer.algorithm().to_owned(),
            transparency_log_index: None,
        },
        apply: ApplyContract {
            // A media package is self-sufficient: the consumer gates it by the cursor GUARD derived
            // from its kind (FirstBaseline / RebaselineForward), not by an exact from-sequence.
            expected_client_from_sequence: PackageSequence::NONE,
            result_sequence: spec.to_sequence,
            requires_empty_generation: true,
            schema_ops_digest: digest_bytes(spec.schema_ops_label),
            operations,
            replace_scopes: Vec::new(),
            preconditions: Preconditions {
                schema_version: jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
                embedding_fingerprint: params.embedding_fingerprint.clone(),
                builder_versions: params.builder_versions.clone(),
                active_baseline_id: None,
                active_generation: None,
            },
            postconditions: Postconditions {
                row_counts,
                table_digests,
            },
            index_build: IndexBuildContract {
                bm25_indexes,
                ivfflat_finalize,
                row_level_maintenance_only: false,
                queryable_before_finalize: false,
            },
            idempotency_key: spec.package_id.clone(),
            rollback_policy: RollbackPolicy::KeepPreviousGenerationUntilValidated,
        },
        payload: PayloadLayout {
            files,
            apply_order: APPLY_ORDER.iter().map(|t| (*t).to_owned()).collect(),
        },
    };

    let manifest_digest = canonical_digest(&manifest)?;
    let signed = Signed::seal(manifest, signer)?;
    let manifest_json = serde_json::to_vec_pretty(&signed)?;
    std::fs::write(artifact::manifest_path(artifact_dir), &manifest_json)?;

    // Seed the producer catalog (the change_seq high-water mark for the next incremental's `lo`).
    let builder_versions_json = serde_json::to_value(&params.builder_versions)?;
    insert_package_catalog_row(
        &mut producer.client()?,
        &PackageCatalogRow {
            corpus,
            package_sequence: i64::try_from(spec.to_sequence.get()).unwrap_or(i64::MAX),
            package_id: &spec.package_id,
            package_kind: spec.package_kind.as_str(),
            baseline_id: &spec.baseline_id,
            generation: &spec.generation,
            // A media package covers the whole window up to its frozen high-water mark:
            // (0, high] for a baseline, (prev.high, high] for a re-baseline.
            included_change_seq_low: spec.included_change_seq_low,
            included_change_seq_high: i64::try_from(change_seq_high).unwrap_or(i64::MAX),
            previous_package_id: spec.previous_package_id.as_deref(),
            previous_package_digest: spec.previous_package_sha256.as_deref(),
            // The package/artifact digest (over the payload) is kept DISTINCT from the manifest digest
            // (plan P3 WARN-3) — they serve different chain-link/integrity roles.
            package_digest: Some(&payload_digest),
            manifest_digest: Some(&manifest_digest),
            schema_version: jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
            embedding_fingerprint: &params.embedding_fingerprint,
            builder_versions: &builder_versions_json,
            status: "built",
        },
    )?;

    Ok(MediaBuildReport {
        package_id: spec.package_id.clone(),
        generation: spec.generation.clone(),
        total_rows,
        included_change_seq_high: change_seq_high,
    })
}

/// The server's major version (e.g. `18`), from `server_version_num / 10000`, read through a generic
/// client so a media build can pin the `COPY (FORMAT binary)` PG-major guard against EITHER the
/// self-managed `ManagedPostgres` or the external producer PostgreSQL. (Mirrors
/// `ManagedPostgres::server_version_major`, but works over the `DbClientSource` seam.)
fn server_version_major<C: GenericClient>(db: &mut C) -> Result<u32, BuildError> {
    let row = db
        .query_one("SELECT current_setting('server_version_num');", &[])
        .map_err(StorageError::PostgresClient)?;
    let raw: String = row.get(0);
    let num: u32 = raw.trim().parse().map_err(|_| {
        BuildError::Storage(StorageError::Generations {
            message: format!("could not parse server_version_num `{}`", raw.trim()),
        })
    })?;
    Ok(num / 10_000)
}

/// An IVFFlat finalize directive at the corpus-sized `lists` derived from the embedding row count
/// (mirrors what [`jurisearch_storage::generations::build_generation_indexes`] does on apply).
fn ivfflat_finalize_for(
    table: &str,
    index: &str,
    row_counts: &BTreeMap<String, u64>,
) -> IvfflatFinalize {
    let count = i64::try_from(row_counts.get(table).copied().unwrap_or(0)).unwrap_or(i64::MAX);
    let lists = recommended_ivfflat_lists(count);
    IvfflatFinalize {
        index: index.to_owned(),
        lists,
        probes: recommended_probes(lists),
    }
}

/// The BM25 (`pg_search`) index names on the replicated tables, so the client's index contract knows
/// which to (re)build. Discovered from `pg_catalog` rather than hard-coded, so a new BM25 index ships
/// without a producer code change.
fn query_bm25_index_names<C: GenericClient>(db: &mut C) -> Result<Vec<String>, BuildError> {
    let any_replicated = REPLICATED_TABLES
        .iter()
        .map(|t| format!("'public.{t}'::regclass"))
        .collect::<Vec<_>>()
        .join(", ");
    let rows = db.query(
        &format!(
            "SELECT i.relname AS name \
             FROM pg_index x \
             JOIN pg_class i ON i.oid = x.indexrelid \
             JOIN pg_am am ON am.oid = i.relam \
             WHERE x.indrelid IN ({any_replicated}) AND am.amname = 'bm25' \
             ORDER BY i.relname;"
        ),
        &[],
    )?;
    Ok(rows
        .iter()
        .map(|row| row.get::<_, String>("name"))
        .collect())
}
