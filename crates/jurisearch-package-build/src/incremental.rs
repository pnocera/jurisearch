//! Incremental builder (plan P4): materialise an ORDERED, GAP-FREE diff for one corpus from the outbox
//! window `(lo, hi]` (frozen under the high-water fence) into JSONL payload files + a signed manifest,
//! and seed the catalog. Changed scopes are COALESCED to one final action per semantic scope with
//! "widest-op-wins" (a `documents`/`chunks` touch ⇒ the BM25-safe `ChunksWithEmbeddings` replace-set;
//! a pure `chunk_embeddings` touch ⇒ the narrow `ChunkEmbeddings`). Rows are rematerialised from the
//! producer's CURRENT authoritative state in the snapshot (the outbox carries scope identity, not row
//! deltas). An empty window builds NO package.
//!
//! Deferred (documented) edges for this vertical slice: graph-edge SHRINKAGE (a `documents` scope
//! re-upserts the document's current edges but does not delete removed ones) and
//! `decision_legislation_citations` deletes (the writer is additive) — both call for a future
//! document-scoped replace-set group.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use jurisearch_package::artifact;
use jurisearch_package::canonical::{HashingWriter, canonical_digest, digest_bytes};
use jurisearch_package::compat::Version;
use jurisearch_package::corpus::Corpus;
use jurisearch_package::event::{
    EventKind, ReplaceSet, ReplaceSetGroup, ReplaceSetOp, ReplaceSetScope, set_digest_over_rows,
};
use jurisearch_package::manifest::embedded::{
    ApplyContract, Compatibility, Compression, EmbeddedManifest, Entitlement, ExtensionRequirement,
    Identity, IndexBuildContract, Integrity, OperationCount, PayloadFile, PayloadFormat,
    PayloadLayout, Postconditions, Preconditions, RollbackPolicy,
};
use jurisearch_package::sequence::PackageSequence;
use jurisearch_package::signed::Signed;
use jurisearch_package::{PACKAGE_FORMAT_VERSION, PackageKind, Signer};

use jurisearch_storage::backend::DbClientSource;
use jurisearch_storage::generations::{primary_key_columns, replicated_table_columns};
use jurisearch_storage::incremental::{replace_set_rows, row_object_select};
use jurisearch_storage::outbox::{
    DigestSource, acquire_outbox_fence, corpus_table_digests_with_client,
    current_change_seq_with_client, release_outbox_fence, scopes_changed_for_corpus_with_client,
};
use jurisearch_storage::package_catalog::{
    LatestPackage, PackageCatalogRow, acquire_corpus_build_lock, insert_package_catalog_row,
    latest_package_for_corpus, release_corpus_build_lock,
};
use jurisearch_storage::runtime::{StorageError, sql_identifier, sql_string_literal};
use postgres::GenericClient;
use serde_json::Value;

use crate::error::BuildError;

/// §5.2/§6.2.2 dependency apply order — same shape as the baseline's (base before derived;
/// official_api_responses before the citation tables; embeddings via their parent's replace-set).
const APPLY_ORDER: &[&str] = &[
    "documents",
    "official_api_responses",
    "graph_edges",
    "legi_metadata_roots",
    "legislation_citation_resolutions",
    "decision_legislation_citations",
    "chunks_with_embeddings",
    "chunk_embeddings",
    "zone_units",
    "decision_zones",
];

/// Producer-supplied inputs (kept explicit so a build is deterministic).
#[derive(Debug, Clone)]
pub struct IncrementalParams {
    pub builder_run_id: String,
    pub created_at: String,
    pub embedding_fingerprint: String,
    pub embedding_model: String,
    pub embedding_dimension: u32,
    pub embedding_normalize: bool,
    pub builder_versions: BTreeMap<String, String>,
    pub minimum_client_version: Version,
}

/// Summary of a built incremental.
#[derive(Debug, Clone)]
pub struct IncrementalBuildReport {
    pub corpus: String,
    pub package_id: String,
    pub from_sequence: u64,
    pub to_sequence: u64,
    pub included_change_seq_low: u64,
    pub included_change_seq_high: u64,
    pub scope_count: usize,
    pub artifact_dir: PathBuf,
}

/// Build an incremental for `corpus` (plan P4). Returns `None` when the outbox window `(lo, hi]` holds
/// no changed scopes (no package is built, the chain does not advance).
///
/// # Errors
/// [`BuildError`] on a DB/IO/canonicalisation/signing failure, or if no baseline exists yet.
pub fn build_incremental(
    producer: &impl DbClientSource,
    corpus: &str,
    artifact_dir: &Path,
    signer: &dyn Signer,
    params: &IncrementalParams,
) -> Result<Option<IncrementalBuildReport>, BuildError> {
    let corpus_typed = Corpus::new(corpus.to_owned())?;
    let mut db = producer.client()?;

    // Serialize per-corpus builds (D1); fence the outbox high-water mark on a DEDICATED connection so it
    // is held only across the snapshot freeze (D7 + WARN-1), not the whole materialisation.
    acquire_corpus_build_lock(&mut db, corpus)?;
    let mut fence_conn = producer.client()?;
    acquire_outbox_fence(&mut fence_conn)?;

    let result = build_incremental_inner(
        &mut db,
        &mut fence_conn,
        corpus,
        &corpus_typed,
        artifact_dir,
        signer,
        params,
    );

    // Release the fence (a no-op if the inner already released after `hi`) + the per-corpus lock.
    let _ = release_outbox_fence(&mut fence_conn);
    let _ = release_corpus_build_lock(&mut db, corpus);
    result
}

#[allow(clippy::too_many_arguments)]
fn build_incremental_inner(
    db: &mut postgres::Client,
    fence_conn: &mut postgres::Client,
    corpus: &str,
    corpus_typed: &Corpus,
    artifact_dir: &Path,
    signer: &dyn Signer,
    params: &IncrementalParams,
) -> Result<Option<IncrementalBuildReport>, BuildError> {
    // The chain link + window low come from the latest catalog row (must exist — a baseline first).
    let prev: LatestPackage = latest_package_for_corpus(db, corpus)?.ok_or_else(|| {
        BuildError::Storage(StorageError::Generations {
            message: format!("no baseline cataloged for corpus `{corpus}`; build a baseline first"),
        })
    })?;
    let lo = prev.included_change_seq_high;

    // Content-compatibility (plan P4 BLOCKER): an ordinary incremental must NOT cross an
    // embedding-fingerprint / builder-versions / schema boundary — that requires a re-baseline (P5),
    // not an incremental. Refuse to BUILD one whose params differ from the corpus's cataloged stamps.
    let params_builder_versions = serde_json::to_value(&params.builder_versions)?;
    if params.embedding_fingerprint != prev.embedding_fingerprint {
        return Err(BuildError::Storage(StorageError::Generations {
            message: format!(
                "incremental embedding_fingerprint `{}` != cataloged `{}`; a re-baseline is required",
                params.embedding_fingerprint, prev.embedding_fingerprint
            ),
        }));
    }
    if params_builder_versions != prev.builder_versions {
        return Err(BuildError::Storage(StorageError::Generations {
            message: "incremental builder_versions differ from the cataloged set; a re-baseline is required"
                .to_owned(),
        }));
    }
    if jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION != prev.schema_version {
        return Err(BuildError::Storage(StorageError::Generations {
            message: format!(
                "incremental schema_version {} != cataloged {}; a re-baseline is required",
                jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
                prev.schema_version
            ),
        }));
    }

    let mut tx = db
        .build_transaction()
        .isolation_level(postgres::IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()?;
    let hi = current_change_seq_with_client(&mut tx)?.get();
    // Snapshot established + `hi` frozen → release the fence so concurrent ingest proceeds (WARN-1).
    release_outbox_fence(fence_conn)?;
    let scopes = scopes_changed_for_corpus_with_client(
        &mut tx,
        corpus,
        jurisearch_package::ChangeSeq::new(u64::try_from(lo).unwrap_or(0)),
        jurisearch_package::ChangeSeq::new(hi),
    )?;
    if scopes.is_empty() {
        tx.commit()?;
        return Ok(None); // empty window: no package, chain does not advance
    }

    // --- Coalesce scopes into final per-scope actions (widest-op-wins) ---
    // NOTE (memory bound): these BTreeSets hold only the UNIQUE changed scope KEYS (strings), so the
    // coalescing memory is O(unique changed scopes) — the row bodies are never held here (they are
    // (re)fetched and STREAMED one scope at a time in the payload sections below). Replacing this with
    // a cursor-based, streaming coalescing pass (so even the key set is not fully materialised) is a
    // deferred follow-up.
    let mut base_keys: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut documents_scoped: BTreeSet<String> = BTreeSet::new();
    let mut chunks_touched: BTreeSet<String> = BTreeSet::new();
    let mut chunk_embeddings_touched: BTreeSet<String> = BTreeSet::new();
    let mut zone_touched: BTreeSet<String> = BTreeSet::new();
    let mut decision_zones_touched: BTreeSet<String> = BTreeSet::new();
    for scope in &scopes {
        match scope.table_name.as_str() {
            "documents" => {
                documents_scoped.insert(scope.scope_key.clone());
                base_keys
                    .entry("documents")
                    .or_default()
                    .insert(scope.scope_key.clone());
            }
            "official_api_responses"
            | "legi_metadata_roots"
            | "legislation_citation_resolutions"
            | "decision_legislation_citations" => {
                base_keys
                    .entry(table_static(&scope.table_name))
                    .or_default()
                    .insert(scope.scope_key.clone());
            }
            "chunks" => {
                chunks_touched.insert(scope.scope_key.clone());
            }
            "chunk_embeddings" => {
                chunk_embeddings_touched.insert(scope.scope_key.clone());
            }
            "zone_units" | "zone_unit_embeddings" => {
                zone_touched.insert(scope.scope_key.clone());
            }
            "decision_zones" => {
                decision_zones_touched.insert(scope.scope_key.clone());
            }
            other => {
                return Err(BuildError::Storage(StorageError::Outbox {
                    message: format!("unknown replicated table in outbox scope: `{other}`"),
                }));
            }
        }
    }
    // A documents scope affects the chunk set (BM25 safety) — widen to ChunksWithEmbeddings.
    let chunks_wide: BTreeSet<String> = chunks_touched.union(&documents_scoped).cloned().collect();
    let chunk_emb_only: BTreeSet<String> = chunk_embeddings_touched
        .difference(&chunks_wide)
        .cloned()
        .collect();

    std::fs::create_dir_all(artifact::payload_dir(artifact_dir))?;

    let mut files: Vec<PayloadFile> = Vec::new();
    let mut per_file_digests: BTreeMap<String, String> = BTreeMap::new();
    let mut operations: Vec<OperationCount> = Vec::new();
    let mut scope_count = 0usize;

    // --- Base-table upserts/deletes ---
    // Each op is STREAMED one row at a time to a lazily-opened `HashingWriter<BufWriter<File>>`, so peak
    // memory is bounded by ONE scope's rows (already per-scope-fetched) plus a fixed buffer — never the
    // whole delta. Ordering (BTreeMap over tables, upsert then delete per table) is load-bearing.
    for (&table, keys) in &base_keys {
        let columns = replicated_table_columns(&mut tx, table)?;
        let pk = primary_key_columns(&mut tx, table)?;
        let mut upserts =
            JsonlOpWriter::new(artifact_dir, table, EventKind::Upsert, columns.clone());
        let mut deletes = JsonlOpWriter::new(artifact_dir, table, EventKind::Delete, pk.clone());
        for key in keys {
            scope_count += 1;
            let rows = read_scope_rows(&mut tx, table, &columns, corpus, key)?;
            if rows.is_empty() {
                // A base row that vanished is a delete (except additive decision_legislation_citations,
                // whose deletes are deferred — emit nothing).
                if table != "decision_legislation_citations" {
                    let delete = delete_key_object(table, &pk, corpus, key);
                    deletes.write_row(&delete)?;
                }
            } else {
                for row in &rows {
                    upserts.write_row(row)?;
                }
            }
            // `rows` dropped here — only one scope's bodies are ever resident.
        }
        upserts.finish(&mut files, &mut per_file_digests, &mut operations)?;
        deletes.finish(&mut files, &mut per_file_digests, &mut operations)?;
    }

    // --- graph_edges: a documents scope re-upserts the document's current edges (no shrinkage) ---
    if !documents_scoped.is_empty() {
        let columns = replicated_table_columns(&mut tx, "graph_edges")?;
        let mut writer = JsonlOpWriter::new(
            artifact_dir,
            "graph_edges",
            EventKind::Upsert,
            columns.clone(),
        );
        for doc in &documents_scoped {
            let select = format!(
                "SELECT {obj} FROM public.graph_edges t WHERE from_document_id = {key} ORDER BY edge_id;",
                obj = row_object_select(&columns),
                key = sql_string_literal(doc),
            );
            let edges = query_json_rows(&mut tx, &select)?;
            for edge in &edges {
                writer.write_row(edge)?;
            }
        }
        writer.finish(&mut files, &mut per_file_digests, &mut operations)?;
    }

    // --- Replace-sets (coalesced, widest-op-wins) ---
    // FIXED group order (load-bearing for the signature). Each group iterates its OWN sorted set and
    // materialises ONE `ReplaceSet` at a time, streaming it as a single line then dropping it — never
    // holding more than one set (or all groups' sets) in memory. `scope_count` increments stay exactly
    // where they were: per doc processed, and NOT for a ChunksWithEmbeddings doc skipped below.
    for (group, docs) in [
        (ReplaceSetGroup::ChunksWithEmbeddings, &chunks_wide),
        (ReplaceSetGroup::ChunkEmbeddings, &chunk_emb_only),
        (ReplaceSetGroup::ZoneUnits, &zone_touched),
        (ReplaceSetGroup::DecisionZones, &decision_zones_touched),
    ] {
        let mut writer = JsonlOpWriter::new(
            artifact_dir,
            group.as_str(),
            EventKind::ReplaceSet,
            Vec::new(),
        );
        for doc in docs {
            // A deleted document's children are cleared by the documents delete cascade — skip its set
            // (only the ChunksWithEmbeddings group can contain a documents-scoped, now-deleted doc).
            if group == ReplaceSetGroup::ChunksWithEmbeddings
                && documents_scoped.contains(doc)
                && !document_exists(&mut tx, corpus, doc)?
            {
                continue;
            }
            scope_count += 1;
            let set = materialize_replace_set(&mut tx, group, corpus, doc, params)?;
            writer.write_row(&set)?;
            // `set` dropped at the end of this iteration — never more than one ReplaceSet resident.
        }
        writer.finish(&mut files, &mut per_file_digests, &mut operations)?;
    }

    // Postconditions over the producer's current corpus (the convergence proof).
    let digests = corpus_table_digests_with_client(&mut tx, corpus, DigestSource::ProducerPublic)?;
    let mut row_counts = BTreeMap::new();
    let mut table_digests = BTreeMap::new();
    for d in &digests {
        row_counts.insert(d.table_name.clone(), d.row_count);
        table_digests.insert(d.table_name.clone(), d.digest.clone());
    }
    let schema_migration_bundle_digest =
        jurisearch_storage::migrations::schema_bundle_digest(&mut tx)?;
    tx.commit()?;

    // --- Manifest + catalog ---
    let from_sequence = PackageSequence::new(u64::try_from(prev.package_sequence).unwrap_or(0));
    let to_sequence = from_sequence.next();
    let package_id = format!("{corpus}-{}-{}", from_sequence.get(), to_sequence.get());
    let payload_digest = artifact::aggregate_payload_digest(&per_file_digests);

    let manifest = EmbeddedManifest {
        identity: Identity {
            package_format_version: PACKAGE_FORMAT_VERSION,
            package_id: package_id.clone(),
            corpus: corpus_typed.clone(),
            package_kind: PackageKind::Incremental,
            from_sequence,
            to_sequence,
            previous_package_id: Some(prev.package_id.clone()),
            previous_package_sha256: prev.package_digest.clone(),
            baseline_id: prev.baseline_id.clone(),
            generation: prev.generation.clone(),
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
            // JSONL incrementals are portable — no PG-major pin (contrast the baseline's CopyBinary).
            postgres_major_min: None,
            postgres_major_max: None,
        },
        entitlement: Entitlement {
            entitlement_corpus: corpus_typed.clone(),
            tier: "all".to_owned(),
            license_epoch: 0,
            audience: None,
            entitlement_policy_digest: digest_bytes(b"p4-open-entitlement"),
        },
        integrity: Integrity {
            artifact_sha256: payload_digest.clone(),
            uncompressed_payload_digest: payload_digest.clone(),
            per_file_digests,
            canonicalisation_algorithm: "jcs-sha256".to_owned(),
            // Descriptive only; the authoritative algorithm is `Signed.signature.algorithm` (plan P6).
            signature_algorithm: signer.algorithm().to_owned(),
            transparency_log_index: None,
        },
        apply: ApplyContract {
            expected_client_from_sequence: from_sequence,
            result_sequence: to_sequence,
            requires_empty_generation: false,
            schema_ops_digest: digest_bytes(b"incremental-no-schema-ops"),
            operations,
            replace_scopes: Vec::new(),
            preconditions: Preconditions {
                schema_version: jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
                embedding_fingerprint: params.embedding_fingerprint.clone(),
                builder_versions: params.builder_versions.clone(),
                active_baseline_id: Some(prev.baseline_id.clone()),
                active_generation: Some(prev.generation.clone()),
            },
            postconditions: Postconditions {
                row_counts,
                table_digests,
            },
            index_build: IndexBuildContract {
                bm25_indexes: Vec::new(),
                ivfflat_finalize: Vec::new(),
                // Ordinary incremental: row-level index maintenance suffices, no finalize (§7.3).
                row_level_maintenance_only: true,
                queryable_before_finalize: true,
            },
            idempotency_key: package_id.clone(),
            rollback_policy: RollbackPolicy::TransactionRollback,
        },
        payload: PayloadLayout {
            files,
            apply_order: APPLY_ORDER.iter().map(|t| (*t).to_owned()).collect(),
        },
    };

    let manifest_digest = canonical_digest(&manifest)?;
    let signed = Signed::seal(manifest, signer)?;
    std::fs::write(
        artifact::manifest_path(artifact_dir),
        serde_json::to_vec_pretty(&signed)?,
    )?;

    let builder_versions_json = serde_json::to_value(&params.builder_versions)?;
    insert_package_catalog_row(
        db,
        &PackageCatalogRow {
            corpus,
            package_sequence: i64::try_from(to_sequence.get()).unwrap_or(i64::MAX),
            package_id: &package_id,
            package_kind: PackageKind::Incremental.as_str(),
            baseline_id: &prev.baseline_id,
            generation: &prev.generation,
            included_change_seq_low: lo,
            included_change_seq_high: i64::try_from(hi).unwrap_or(i64::MAX),
            previous_package_id: Some(&prev.package_id),
            previous_package_digest: prev.package_digest.as_deref(),
            package_digest: Some(&payload_digest),
            manifest_digest: Some(&manifest_digest),
            schema_version: jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
            embedding_fingerprint: &params.embedding_fingerprint,
            builder_versions: &builder_versions_json,
            status: "built",
        },
    )?;

    Ok(Some(IncrementalBuildReport {
        corpus: corpus.to_owned(),
        package_id,
        from_sequence: from_sequence.get(),
        to_sequence: to_sequence.get(),
        included_change_seq_low: u64::try_from(lo).unwrap_or(0),
        included_change_seq_high: hi,
        scope_count,
        artifact_dir: artifact_dir.to_owned(),
    }))
}

/// A `'static` table name for `base_keys` keys (the four base-upsert tables besides `documents`).
fn table_static(table: &str) -> &'static str {
    match table {
        "official_api_responses" => "official_api_responses",
        "legi_metadata_roots" => "legi_metadata_roots",
        "legislation_citation_resolutions" => "legislation_citation_resolutions",
        "decision_legislation_citations" => "decision_legislation_citations",
        _ => "documents",
    }
}

/// Read the current authoritative row(s) for a base-table scope, as JSON objects (non-gen columns).
fn read_scope_rows<C: GenericClient>(
    tx: &mut C,
    table: &str,
    columns: &[String],
    corpus: &str,
    scope_key: &str,
) -> Result<Vec<Value>, StorageError> {
    let obj = row_object_select(columns);
    let key = sql_string_literal(scope_key);
    let corpus_lit = sql_string_literal(corpus);
    let predicate = match table {
        "documents" => format!("document_id = {key}"),
        "official_api_responses" => format!("response_id = {key}"),
        "legi_metadata_roots" => format!("metadata_key = {key}"),
        "legislation_citation_resolutions" => {
            format!("corpus = {corpus_lit} AND citation_key = {key}")
        }
        "decision_legislation_citations" => format!("decision_document_id = {key}"),
        other => {
            return Err(StorageError::Outbox {
                message: format!("no scope predicate for base table `{other}`"),
            });
        }
    };
    let order = primary_key_columns(tx, table)?
        .iter()
        .map(|c| format!("t.{}", sql_identifier(c)))
        .collect::<Vec<_>>()
        .join(", ");
    let select = format!(
        "SELECT {obj} FROM public.{} t WHERE {predicate} ORDER BY {order};",
        sql_identifier(table)
    );
    query_json_rows(tx, &select)
}

/// The delete-key JSON object (the PK reconstructed from the scope key + corpus for the multi-col PK).
fn delete_key_object(table: &str, pk: &[String], corpus: &str, scope_key: &str) -> Value {
    let mut obj = serde_json::Map::new();
    if table == "legislation_citation_resolutions" {
        obj.insert("corpus".to_owned(), Value::String(corpus.to_owned()));
        obj.insert(
            "citation_key".to_owned(),
            Value::String(scope_key.to_owned()),
        );
    } else if let Some(col) = pk.first() {
        obj.insert(col.clone(), Value::String(scope_key.to_owned()));
    }
    Value::Object(obj)
}

fn document_exists<C: GenericClient>(
    tx: &mut C,
    _corpus: &str,
    document_id: &str,
) -> Result<bool, StorageError> {
    let exists: bool = tx
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM public.documents WHERE document_id = $1);",
            &[&document_id],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    Ok(exists)
}

/// Materialise one replace-set scope from the producer's CURRENT derived set.
fn materialize_replace_set<C: GenericClient>(
    tx: &mut C,
    group: ReplaceSetGroup,
    corpus: &str,
    document_id: &str,
    params: &IncrementalParams,
) -> Result<ReplaceSet, BuildError> {
    // Read the producer's CURRENT derived set via the SHARED storage reader (the same one the applier
    // uses against the generation), so producer + consumer `set_digest`s are computed over identical rows.
    let rows = replace_set_rows(tx, "public", group, document_id)?;
    let mut row_pks: Vec<String> = Vec::new();
    if let Some(parent_rows) = rows.get(primary_pk_table(group)) {
        for row in parent_rows {
            if let Some(pk) = row.get(group_pk_column(group)).and_then(Value::as_str) {
                row_pks.push(pk.to_owned());
            }
        }
    }
    let set_digest = set_digest_over_rows(&rows);
    let scope = ReplaceSetScope {
        document_id: document_id.to_owned(),
        corpus: Some(Corpus::new(corpus.to_owned())?),
    };
    Ok(ReplaceSet {
        op: ReplaceSetOp::ReplaceSet,
        table_group: group,
        scope,
        rows,
        row_pks,
        builder_version: params
            .builder_versions
            .get("chunker")
            .or_else(|| params.builder_versions.values().next())
            .cloned()
            .unwrap_or_else(|| "unknown".to_owned()),
        source_text_hash: None,
        embedding_fingerprint: params.embedding_fingerprint.clone(),
        set_digest,
    })
}

fn primary_pk_table(group: ReplaceSetGroup) -> &'static str {
    match group {
        ReplaceSetGroup::ZoneUnits => "zone_units",
        ReplaceSetGroup::ChunksWithEmbeddings => "chunks",
        ReplaceSetGroup::ChunkEmbeddings => "chunk_embeddings",
        ReplaceSetGroup::DecisionZones => "decision_zones",
    }
}

fn group_pk_column(group: ReplaceSetGroup) -> &'static str {
    match group {
        ReplaceSetGroup::ZoneUnits => "zone_unit_id",
        ReplaceSetGroup::ChunksWithEmbeddings => "chunk_id",
        ReplaceSetGroup::ChunkEmbeddings => "chunk_id",
        ReplaceSetGroup::DecisionZones => "document_id",
    }
}

fn query_json_rows<C: GenericClient>(tx: &mut C, sql: &str) -> Result<Vec<Value>, StorageError> {
    let rows = tx.query(sql, &[]).map_err(StorageError::PostgresClient)?;
    Ok(rows.iter().map(|row| row.get::<_, Value>(0)).collect())
}

/// Streams ONE JSONL payload op — the file for a single `(table_or_group, op)` — row by row to disk
/// while hashing, so peak memory is bounded by one row plus a fixed `BufWriter` buffer rather than the
/// whole op's rows. It is byte-for-byte equivalent to the previous buffered `rows.map(to_string).join`
/// path: each row is `serde_json::to_writer` (serde's compact formatter, identical to `to_string`)
/// followed by a `\n`, giving the exact `l1\n…lN\n` layout the signed manifest is computed over.
///
/// LAZY OPEN: the file (and therefore the `PayloadFile`, per-file digest, and `OperationCount`) is
/// created only on the FIRST row. An op with zero rows produces NO file and NO metadata — matching the
/// old `write_jsonl_op`'s `if rows.is_empty() { return }`, so no 0-byte artifacts appear.
struct JsonlOpWriter<'a> {
    artifact_dir: &'a Path,
    table_or_group: String,
    op: EventKind,
    /// `PayloadFile.columns` for this op (the table's columns for base/edge ops, empty for replace-set
    /// groups) — recorded verbatim at finalize time.
    columns: Vec<String>,
    /// Set together with `writer` on first row (their `Some`/`None` state is kept in lock-step).
    file_name: Option<String>,
    writer: Option<HashingWriter<BufWriter<File>>>,
    row_count: u64,
}

impl<'a> JsonlOpWriter<'a> {
    fn new(
        artifact_dir: &'a Path,
        table_or_group: &str,
        op: EventKind,
        columns: Vec<String>,
    ) -> Self {
        Self {
            artifact_dir,
            table_or_group: table_or_group.to_owned(),
            op,
            columns,
            file_name: None,
            writer: None,
            row_count: 0,
        }
    }

    /// Open the payload file on first use (idempotent), returning the streaming writer. Uses the same
    /// `incremental_file_name` / `payload_file_path` naming the buffered path used.
    fn writer_mut(&mut self) -> Result<&mut HashingWriter<BufWriter<File>>, BuildError> {
        if self.writer.is_none() {
            let file_name = artifact::incremental_file_name(&self.table_or_group, self.op);
            let path = artifact::payload_file_path(self.artifact_dir, &file_name);
            let file = File::create(&path)?;
            self.writer = Some(HashingWriter::new(BufWriter::new(file)));
            self.file_name = Some(file_name);
        }
        Ok(self.writer.as_mut().expect("writer just set"))
    }

    /// Stream one row as a compact JSON line + `\n`, opening the file on the first call and counting it.
    fn write_row<T: Serialize>(&mut self, row: &T) -> Result<(), BuildError> {
        let writer = self.writer_mut()?;
        serde_json::to_writer(&mut *writer, row)?;
        writer.write_all(b"\n")?;
        self.row_count += 1;
        Ok(())
    }

    /// Finalize: if the op never opened (zero rows), emit NOTHING. Otherwise flush the `BufWriter`
    /// (surfacing any deferred write error) BEFORE trusting the digest — mirroring baseline.rs — then
    /// record the `PayloadFile` + per-file digest + `OperationCount` in the same sequence the old
    /// `write_jsonl_bytes` did (digest insert, op push, file push).
    fn finish(
        self,
        files: &mut Vec<PayloadFile>,
        per_file_digests: &mut BTreeMap<String, String>,
        operations: &mut Vec<OperationCount>,
    ) -> Result<(), BuildError> {
        let Some(mut writer) = self.writer else {
            return Ok(());
        };
        let file_name = self
            .file_name
            .expect("file_name is set whenever writer is set");
        // Surface any deferred BufWriter error before relying on the digest/file (Drop would swallow it).
        writer.flush()?;
        let digest = writer.finalize();
        per_file_digests.insert(file_name.clone(), digest.clone());
        operations.push(OperationCount {
            table: self.table_or_group.clone(),
            op: self.op,
            count: self.row_count,
        });
        files.push(PayloadFile {
            file_name,
            table: self.table_or_group,
            columns: self.columns,
            op: self.op,
            format: PayloadFormat::Jsonl,
            compression: Compression::None,
            row_count: self.row_count,
            digest,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write;

    use jurisearch_package::canonical::{HashingWriter, digest_bytes};
    use jurisearch_package::event::{ReplaceSet, ReplaceSetGroup, ReplaceSetOp, ReplaceSetScope};

    /// Prove that streaming each row with the exact two lines `JsonlOpWriter::write_row` runs
    /// (`serde_json::to_writer` + a trailing `\n`) over a `HashingWriter` is BYTE-identical AND
    /// DIGEST-identical to the pre-refactor buffered algorithm `map(to_string).join("\n") + "\n"`.
    fn assert_old_eq_new<T: serde::Serialize>(set: &[T]) {
        // OLD (pre-refactor buffered path):
        let old = set
            .iter()
            .map(serde_json::to_string)
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n")
            + "\n";
        let old_digest = digest_bytes(old.as_bytes());

        // NEW (streaming path — the exact two lines JsonlOpWriter::write_row runs):
        let mut hw = HashingWriter::new(Vec::<u8>::new());
        for row in set {
            serde_json::to_writer(&mut hw, row).unwrap();
            hw.write_all(b"\n").unwrap();
        }
        let new_bytes = hw.get_mut().clone();
        let new_digest = hw.finalize();

        assert_eq!(new_bytes, old.as_bytes());
        assert_eq!(new_digest, old_digest);
    }

    #[test]
    fn streaming_jsonl_writer_is_byte_and_digest_identical_to_old_join_algorithm() {
        // 1. A MULTI-row set of nested objects + arrays with varied, tricky content (quotes,
        //    slashes, unicode, numbers, nulls, bools) — exercises the `\n` BETWEEN lines.
        let multi: Vec<serde_json::Value> = vec![
            serde_json::json!({
                "nested": {"a": 1, "b": [true, false, null]},
                "text": "quote\" and /slash\\ and é/ü unicode ☃",
                "num": -3.5,
                "arr": [1, 2, {"deep": [null, "x"]}],
            }),
            serde_json::json!({
                "s": "line two",
                "empty_obj": {},
                "empty_arr": [],
                "flag": false,
                "n": null,
            }),
            serde_json::json!([{"k": "v"}, 42, "trailing"]),
        ];
        assert_old_eq_new(&multi);

        // 2. A SINGLE-row set.
        let single: Vec<serde_json::Value> = vec![serde_json::json!({"only": "row", "x": [1, 2]})];
        assert_old_eq_new(&single);

        // 3. A set of `ReplaceSet` envelopes: one with a NON-empty nested `rows` map, and one with
        //    an EMPTY nested `rows` map (+ empty `row_pks`) — the "empty envelope is still ONE line"
        //    case (both `rows` and `row_pks` have `skip_serializing_if`, so they are omitted).
        let replace_sets: Vec<ReplaceSet> = vec![
            ReplaceSet {
                op: ReplaceSetOp::ReplaceSet,
                table_group: ReplaceSetGroup::ChunksWithEmbeddings,
                scope: ReplaceSetScope {
                    document_id: "cass:D1".to_owned(),
                    corpus: None,
                },
                rows: BTreeMap::from([
                    (
                        "chunks".to_owned(),
                        vec![serde_json::json!({"chunk_id": "cass:D1#0", "body": "premier moyen"})],
                    ),
                    (
                        "chunk_embeddings".to_owned(),
                        vec![
                            serde_json::json!({"chunk_id": "cass:D1#0", "embedding": [0.1, 0.2, 0.3]}),
                        ],
                    ),
                ]),
                row_pks: vec!["cass:D1#0".to_owned()],
                builder_version: "c1".to_owned(),
                source_text_hash: None,
                embedding_fingerprint: "fp".to_owned(),
                set_digest: "sha256:deadbeef".to_owned(),
            },
            ReplaceSet {
                op: ReplaceSetOp::ReplaceSet,
                table_group: ReplaceSetGroup::ChunkEmbeddings,
                scope: ReplaceSetScope {
                    document_id: "cass:D2".to_owned(),
                    corpus: None,
                },
                rows: BTreeMap::new(),
                row_pks: vec![],
                builder_version: "c1".to_owned(),
                source_text_hash: Some("sha256:abc".to_owned()),
                embedding_fingerprint: "fp".to_owned(),
                set_digest: "sha256:cafef00d".to_owned(),
            },
        ];
        assert_old_eq_new(&replace_sets);
    }
}
