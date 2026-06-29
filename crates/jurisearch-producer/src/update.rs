//! The in-process `update` orchestration: (fetch) → ingest → enrich → embed → `producer_cycle("core")`
//! → signed manifest, driven over an EXTERNAL `DbClientSource` (never `ManagedPostgres`).
//!
//! The DB-mutating span (ingest → … → cycle) runs under the single `update-core` lock; only the pure
//! network download runs outside it. Publish is exactly-once / no-partial: `producer_cycle` publishes
//! each new incremental BEFORE the manifest references it, and an empty outbox window still refreshes the
//! signed manifest and exits zero (an empty run is a SUCCESSFUL run).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use jurisearch_core::error::ErrorCode;
use jurisearch_fetch::ArchiveSource;
use jurisearch_official_api::PisteClient;
use jurisearch_package::compat::Version;
use jurisearch_package::manifest::remote::{CatchupPolicy, EntitlementTier};
use jurisearch_package_build::{
    EnrichmentMode, IncrementalParams, ProducerCycleConfig, ProducerCycleReport,
    RemoteManifestParams, producer_cycle,
};
use jurisearch_pipeline::embedding::EmbeddingPoolEndpoint;
use jurisearch_pipeline::{
    ArchiveSyncFilter, EmbedRequest, EmbedTarget, EnrichRequest, IngestArchivesRequest,
    embed_documents, enrich_zones, ingest_archives,
};
use jurisearch_storage::backend::{DbClientSource, WriterHandle};
use jurisearch_storage::zone_units::EnrichZoneOrder;

use crate::config::{EnrichmentModeConfig, ProducerConfig};
use crate::cursors::{
    FetchCursorCoordinate, IngestJournalCoordinate, PackageHighWaterMark, RunCheckpoint, RunPhase,
};
use crate::error::ProducerError;
use crate::fetch::{fetch_source, read_fetch_cursor};
use crate::lock::acquire_update_lock;
use crate::timestamp::now_rfc3339;

/// Upper bound on a single archive member's uncompressed bytes streamed into storage.
const MAX_MEMBER_BYTES: u64 = 256 * 1024 * 1024;
/// Embedding batch + pool sizing for the document embed passes.
const EMBED_BATCH_SIZE: usize = 32;
const EMBED_POOL_CONCURRENCY: usize = 4;
/// Bounded wait for the `update-core` lock before a run reports `skipped-lock-held`.
const DEFAULT_LOCK_WAIT: Duration = Duration::from_secs(900);

/// Operator knobs for one `update` invocation.
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    pub group: String,
    pub dry_run: bool,
    pub skip_fetch: bool,
    pub skip_enrich: bool,
    pub lock_wait: Duration,
}

impl UpdateOptions {
    #[must_use]
    pub fn new(group: impl Into<String>) -> Self {
        Self {
            group: group.into(),
            dry_run: false,
            skip_fetch: false,
            skip_enrich: false,
            lock_wait: DEFAULT_LOCK_WAIT,
        }
    }
}

/// What one `update` run produced. `exit_class` classifies the outcome for a timer/alert wrapper.
#[derive(Debug, Clone)]
pub struct UpdateReport {
    pub group: String,
    pub run_id: String,
    pub sources: Vec<String>,
    pub dry_run: bool,
    pub fetch_cursors: Vec<FetchCursorCoordinate>,
    pub ingest_journals: Vec<IngestJournalCoordinate>,
    pub enrichment: EnrichmentMode,
    /// The package id built this cycle, or `None` if the outbox window was empty (a no-op publish).
    pub built_incremental: Option<String>,
    pub package_high_water_mark: Option<PackageHighWaterMark>,
    pub exit_class: &'static str,
}

/// Classify a completed `producer_cycle` outcome for the exit-class taxonomy. An empty window (no
/// incremental, manifest still refreshed) is a SUCCESSFUL no-op, never a failure.
#[must_use]
pub fn classify_cycle(report: &ProducerCycleReport) -> &'static str {
    match (&report.built_incremental, &report.enrichment) {
        (None, _) => "no-op",
        (Some(_), EnrichmentMode::SkippedNoCredentials) => "published-enrich-degraded",
        (Some(_), _) => "published",
    }
}

/// Run the full `update` orchestration for a fetch group. See the module docs for the lock/ordering
/// contract.
pub fn run_update(
    config: &ProducerConfig,
    options: &UpdateOptions,
) -> Result<UpdateReport, ProducerError> {
    let sources = config.resolve_group(&options.group)?;
    let run_id = format!(
        "{}-{}",
        options.group,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );
    let state_dir = &config.producer.state_dir;
    let mut checkpoint = RunCheckpoint::started(&options.group, &run_id);
    checkpoint.save(state_dir)?;

    // --- Phase 1: fetch (network only, NO DB writes → runs OUTSIDE the update lock). ---
    let mut fetch_cursors = Vec::new();
    for &source in &sources {
        let report = if options.skip_fetch {
            read_fetch_cursor(config, source)?
        } else {
            fetch_source(config, source, options.dry_run)?.cursor
        };
        fetch_cursors.push(report);
    }
    checkpoint.phase = RunPhase::Fetched;
    checkpoint.fetch_cursors = fetch_cursors.clone();
    checkpoint.save(state_dir)?;

    // A dry run stops here: it never opens the DB, takes no lock, and publishes nothing.
    if options.dry_run {
        return Ok(UpdateReport {
            group: options.group.clone(),
            run_id,
            sources: sources.iter().map(|s| s.as_str().to_owned()).collect(),
            dry_run: true,
            fetch_cursors,
            ingest_journals: Vec::new(),
            enrichment: EnrichmentMode::Disabled,
            built_incremental: None,
            package_high_water_mark: None,
            exit_class: "dry-run",
        });
    }

    // --- Acquire the single core update lock for the entire DB-mutating span. ---
    let _lock = acquire_update_lock(state_dir, options.lock_wait)?;
    let db = config.writer_handle()?;
    ensure_provisioned(&db)?;

    // --- Phase 2/3: ingest each source's newly-mirrored archives. ---
    let mut ingest_journals = Vec::new();
    for &source in &sources {
        let journal = ingest_one(config, &db, source)?;
        ingest_journals.push(journal);
    }
    checkpoint.phase = RunPhase::Ingested;
    checkpoint.ingest_journals = ingest_journals.clone();
    checkpoint.save(state_dir)?;

    // --- Phase 4: enrich (Judilibre covers cass/inca). Honest skip with no creds. ---
    let enrichment =
        if options.skip_enrich || config.enrichment.mode == EnrichmentModeConfig::Disabled {
            EnrichmentMode::Disabled
        } else {
            enrich_group(config, &db, &sources)?
        };
    checkpoint.phase = RunPhase::Enriched;
    checkpoint.save(state_dir)?;

    // --- Phase 5: embed pending documents + zone units (document embedding over public text). ---
    embed_pending(config, &db, EmbedTarget::Chunks)?;
    embed_pending(config, &db, EmbedTarget::ZoneUnits)?;
    checkpoint.phase = RunPhase::Embedded;
    checkpoint.save(state_dir)?;

    // --- Phase 6: producer_cycle("core") — build (if any) + publish + refresh signed manifest. ---
    let cycle = run_cycle(config, &db, &run_id, enrichment.clone())?;
    // Record the REAL package coordinates the cycle published (WARN fix): the published head sequence and
    // its frozen `change_seq` window-high — not the previous all-`None` placeholder. For an empty outbox
    // these reflect the current published head (the included window-high is unchanged).
    let hwm = PackageHighWaterMark {
        corpus: config.package.corpus.clone(),
        head_sequence: cycle.head_sequence,
        included_change_seq_high: cycle.included_change_seq_high,
    };
    checkpoint.phase = RunPhase::Published;
    checkpoint.package_high_water_mark = Some(hwm.clone());
    checkpoint.save(state_dir)?;

    let exit_class = classify_cycle(&cycle);
    Ok(UpdateReport {
        group: options.group.clone(),
        run_id,
        sources: sources.iter().map(|s| s.as_str().to_owned()).collect(),
        dry_run: false,
        fetch_cursors,
        ingest_journals,
        enrichment: cycle.enrichment,
        built_incremental: cycle.built_incremental,
        package_high_water_mark: Some(hwm),
        exit_class,
    })
}

/// Fail with a clear `producer-db-unprovisioned` diagnostic when the external DB has no schema yet,
/// instead of a raw SQL error deep in ingest.
fn ensure_provisioned(db: &WriterHandle) -> Result<(), ProducerError> {
    let mut client = db.client()?;
    let provisioned: bool = client
        .query_one("SELECT to_regclass('public.documents') IS NOT NULL;", &[])
        .map_err(|source| {
            ProducerError::Storage(jurisearch_storage::runtime::StorageError::PostgresClient(
                source,
            ))
        })?
        .get(0);
    if provisioned {
        Ok(())
    } else {
        Err(ProducerError::Unprovisioned(
            "public.documents is absent".to_owned(),
        ))
    }
}

/// Ingest one source's mirrored archives. Selection/idempotency is by DILA archive name/timestamp + the
/// per-archive ingest journal — NEVER by package `change_seq`.
fn ingest_one(
    config: &ProducerConfig,
    db: &impl DbClientSource,
    source: ArchiveSource,
) -> Result<IngestJournalCoordinate, ProducerError> {
    let mirror_dir = config.producer.archives_dir.join(source.as_str());
    let quarantine_dir = config.producer.state_dir.join("ingest-quarantine");
    let req = IngestArchivesRequest {
        source,
        archives_dir: &mirror_dir,
        run_id: None,
        limit_members: None,
        max_member_bytes: MAX_MEMBER_BYTES,
        quarantine_dir: Some(&quarantine_dir),
        safe_mode: false,
        // Selection keys on the DILA archive timestamp via the planner + the journal's de-dup of
        // already-processed file names; it does not consult `change_seq`.
        filter: ArchiveSyncFilter {
            incremental: false,
            since_compact: None,
        },
    };
    let report = ingest_archives(db, req)?;
    Ok(IngestJournalCoordinate {
        source: source.as_str().to_owned(),
        run_id: Some(report.run_id),
        journal_compact_timestamp: report.journal_cursor,
        archives_ingested: report.archives_ingested,
    })
}

/// Run Judilibre zone enrichment for the cass/inca sources present in the group. Honest skip
/// (`SkippedNoCredentials`) when no PISTE creds are present.
fn enrich_group(
    config: &ProducerConfig,
    db: &impl DbClientSource,
    sources: &[ArchiveSource],
) -> Result<EnrichmentMode, ProducerError> {
    let api_config = jurisearch_official_api::OfficialApiConfig::from_env();
    let has_creds = api_config.judilibre_key_id.is_some();
    let piste = has_creds.then(|| PisteClient::new(api_config));

    let mut total_enriched = 0u64;
    let mut any_attempted = false;
    for &source in sources {
        if !matches!(source, ArchiveSource::Cass | ArchiveSource::Inca) {
            continue; // Judilibre covers only the Cour de cassation.
        }
        any_attempted = true;
        let outcome = enrich_zones(
            db,
            piste.as_ref(),
            EnrichRequest {
                source: source.as_str(),
                limit: None,
                since: None,
                concurrency: config.fetch.max_concurrency.max(1) as usize,
                order: EnrichZoneOrder::Oldest,
            },
        )?;
        match outcome.mode {
            jurisearch_pipeline::EnrichmentMode::Ran { zones_enriched } => {
                total_enriched += zones_enriched;
            }
            jurisearch_pipeline::EnrichmentMode::SkippedNoCredentials => {
                return Ok(EnrichmentMode::SkippedNoCredentials);
            }
        }
    }
    if !any_attempted {
        Ok(EnrichmentMode::Disabled)
    } else {
        Ok(EnrichmentMode::Ran {
            zones_enriched: total_enriched,
        })
    }
}

/// Embed the pending set for a target. A "no rows pending" outcome is a NO-OP (an empty run is a
/// success), not a failure — only genuine endpoint/DB/fingerprint failures propagate.
fn embed_pending(
    config: &ProducerConfig,
    db: &impl DbClientSource,
    target: EmbedTarget,
) -> Result<(), ProducerError> {
    let embedding_config = config.embedding_config();
    let api_key = config
        .embedding
        .api_key_env
        .as_deref()
        .and_then(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty());
    // Carry the provider request_model on the endpoint, SEPARATE from the storage fingerprint.
    let endpoint = EmbeddingPoolEndpoint {
        base_url: config.embedding.base_url.clone(),
        request_model: embedding_config.request_model.clone(),
        api_key_env: config.embedding.api_key_env.clone(),
        api_key,
    };
    let req = EmbedRequest {
        target,
        limit: None,
        index_lists: 0, // auto-scale the ivfflat lists to the corpus size.
        batch_size: EMBED_BATCH_SIZE,
        pool_concurrency: EMBED_POOL_CONCURRENCY,
        pool_endpoints: vec![endpoint],
    };
    match embed_documents(db, &embedding_config, req) {
        Ok(_) => Ok(()),
        Err(err) if err.error_object().code == ErrorCode::NoResults => Ok(()),
        Err(err) => Err(ProducerError::Embed(err)),
    }
}

/// Build the [`ProducerCycleConfig`] for one cycle from the producer config. The incremental
/// `embedding_fingerprint` is the STORAGE fingerprint (provider `request_model` excluded), and the
/// remote-manifest signing key id comes from the signer. Public so it can be exercised directly.
#[must_use]
pub fn cycle_config(
    config: &ProducerConfig,
    run_id: &str,
    enrichment: EnrichmentMode,
    signer: &jurisearch_package::crypto::Ed25519Signer,
) -> ProducerCycleConfig {
    ProducerCycleConfig {
        incremental_params: incremental_params(config, run_id),
        remote_manifest_params: remote_manifest_params(config, signer),
        enrichment,
    }
}

/// Run one `producer_cycle("core")` over the served root. The cycle stages built incrementals under the
/// served root's `.staging/pending` slot (crash-resumable), so no separate scratch dir is threaded here.
fn run_cycle(
    config: &ProducerConfig,
    db: &impl DbClientSource,
    run_id: &str,
    enrichment: EnrichmentMode,
) -> Result<ProducerCycleReport, ProducerError> {
    let signer = config.signer()?;
    let cycle_config = cycle_config(config, run_id, enrichment, &signer);
    let report = producer_cycle(
        db,
        &config.package.corpus,
        &config.producer.corpora_dir,
        &signer,
        &cycle_config,
    )?;
    Ok(report)
}

/// The incremental build params, derived from the producer config. `embedding_fingerprint` is the
/// STORAGE fingerprint (request_model excluded), matching the cataloged baseline.
fn incremental_params(config: &ProducerConfig, run_id: &str) -> IncrementalParams {
    let embedding = config.embedding_config();
    let mut builder_versions = BTreeMap::new();
    builder_versions.insert(
        "jurisearch-producer".to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    IncrementalParams {
        builder_run_id: run_id.to_owned(),
        created_at: now_rfc3339(),
        embedding_fingerprint: embedding.storage_embedding_fingerprint(),
        embedding_model: embedding.model.clone(),
        embedding_dimension: embedding.dimension as u32,
        embedding_normalize: embedding.normalize,
        builder_versions,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

/// The remote-manifest params, derived from the producer config (single `core` corpus, open tier).
fn remote_manifest_params(
    config: &ProducerConfig,
    signer: &jurisearch_package::crypto::Ed25519Signer,
) -> RemoteManifestParams {
    RemoteManifestParams {
        publisher: config.producer.publisher.clone(),
        environment: config.producer.environment.clone(),
        generated_at: now_rfc3339(),
        catchup_policy: CatchupPolicy {
            max_incremental_packages: 120,
            max_cumulative_diff_to_baseline_permille: 330,
            max_cumulative_uncompressed_to_baseline_permille: 500,
            max_apply_seconds_budget: 2700,
        },
        entitlement_tier: EntitlementTier::Open,
        license_epoch: 0,
        audience: None,
        signing_key_id: signer.key_id().clone(),
        uri_base: config.package.uri_base.clone(),
        max_retained_incrementals: config.package.max_retained_incrementals,
        default_apply_seconds: 5,
        default_load_seconds: 600,
    }
}

/// True if `path` is under the producer's served root (small guard reused by tests/diagnostics).
#[must_use]
pub fn is_under(root: &Path, candidate: &Path) -> bool {
    candidate.starts_with(root)
}
