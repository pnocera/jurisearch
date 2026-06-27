//! Baseline builder (plan P3, workstream P3-baseline): materialise a `baseline` artifact for one
//! corpus from the producer's authoritative `public` tables — per-table `COPY (FORMAT binary)` payload
//! files + a signed embedded manifest carrying the apply contract, postcondition digests, and index
//! contract — and seed the producer catalog. Signing is stubbed behind the `Signer` trait (real in P6).

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use jurisearch_package::artifact;
use jurisearch_package::canonical::{canonical_digest, digest_bytes};
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

use jurisearch_storage::dense::{
    DENSE_VECTOR_INDEX_NAME, recommended_ivfflat_lists, recommended_probes,
};
use jurisearch_storage::generations::{
    REPLICATED_TABLES, baseline_copy_out_select, generation_name, replicated_table_columns,
};
use jurisearch_storage::outbox::{
    DigestSource, corpus_table_digests_with_client, current_change_seq_with_client,
};
use jurisearch_storage::package_catalog::{PackageCatalogRow, write_package_catalog_row};
use jurisearch_storage::runtime::ManagedPostgres;
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

/// Build a baseline artifact for `corpus` into `artifact_dir` and seed the producer catalog.
///
/// # Errors
/// [`BuildError`] on a DB, IO, canonicalisation, or signing failure.
pub fn build_baseline(
    producer: &ManagedPostgres,
    corpus: &str,
    artifact_dir: &Path,
    signer: &dyn Signer,
    params: &BaselineParams,
) -> Result<BaselineBuildReport, BuildError> {
    let corpus_typed = Corpus::new(corpus.to_owned())?;
    // P3 first baseline: per-corpus package sequence starts at 1.
    let sequence = PackageSequence::new(1);
    let package_id = format!("{corpus}-{}-{}", sequence.get(), sequence.get());
    let generation = generation_name(corpus, 1);

    std::fs::create_dir_all(artifact::payload_dir(artifact_dir))?;

    // BLOCKER fix: cut the ENTIRE baseline from ONE producer snapshot. A single REPEATABLE READ,
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
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        drop(reader);

        let file_name = artifact::baseline_file_name(table);
        std::fs::write(
            artifact::payload_file_path(artifact_dir, &file_name),
            &bytes,
        )?;
        let file_digest = digest_bytes(&bytes);
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

    // The server major is a static property of the instance (not snapshot-sensitive).
    let server_major = producer.server_version_major()?;

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
            package_id: package_id.clone(),
            corpus: corpus_typed.clone(),
            package_kind: PackageKind::Baseline,
            from_sequence: sequence,
            to_sequence: sequence,
            previous_package_id: None,
            previous_package_sha256: None,
            baseline_id: params.baseline_id.clone(),
            generation: generation.clone(),
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
            entitlement_policy_digest: digest_bytes(b"p3-open-entitlement"),
        },
        integrity: Integrity {
            artifact_sha256: payload_digest.clone(),
            uncompressed_payload_digest: payload_digest.clone(),
            per_file_digests,
            canonicalisation_algorithm: "jcs-sha256".to_owned(),
            signature_algorithm: "stub".to_owned(),
            transparency_log_index: None,
        },
        apply: ApplyContract {
            expected_client_from_sequence: PackageSequence::NONE,
            result_sequence: sequence,
            requires_empty_generation: true,
            schema_ops_digest: digest_bytes(b"baseline-no-schema-ops"),
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
            idempotency_key: package_id.clone(),
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

    // Seed the producer catalog (the change_seq high-water mark for the first incremental's `lo`).
    let builder_versions_json = serde_json::to_value(&params.builder_versions)?;
    write_package_catalog_row(
        producer,
        &PackageCatalogRow {
            corpus,
            package_sequence: i64::try_from(sequence.get()).unwrap_or(i64::MAX),
            package_id: &package_id,
            package_kind: PackageKind::Baseline.as_str(),
            baseline_id: &params.baseline_id,
            generation: &generation,
            // A baseline covers the whole window up to its frozen high-water mark: (0, high].
            included_change_seq_low: 0,
            included_change_seq_high: i64::try_from(change_seq_high).unwrap_or(i64::MAX),
            previous_package_id: None,
            previous_package_digest: None,
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

    Ok(BaselineBuildReport {
        corpus: corpus.to_owned(),
        package_id,
        generation,
        baseline_id: params.baseline_id.clone(),
        artifact_dir: artifact_dir.to_owned(),
        total_rows,
        included_change_seq_high: change_seq_high,
    })
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
