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
use jurisearch_fetch::{ArchiveSource, FetchCursor, ParsedArchive};
use jurisearch_official_api::PisteClient;
use jurisearch_package::compat::Version;
use jurisearch_package::manifest::remote::{CatchupPolicy, EntitlementTier};
use jurisearch_package_build::{
    BaselineParams, EnrichmentMode, IncrementalParams, ProducerCycleConfig, ProducerCycleReport,
    RebaselineCycleConfig, RebaselineCycleReport, RemoteManifestParams, producer_cycle,
    rebaseline_cycle,
};
use jurisearch_pipeline::embedding::EmbeddingPoolEndpoint;
use jurisearch_pipeline::{
    ArchiveSyncFilter, EmbedRequest, EmbedTarget, EnrichRequest, IngestArchivesRequest,
    embed_documents, enrich_zones, ingest_archives,
};
use jurisearch_storage::backend::{DbClientSource, WriterHandle};
use jurisearch_storage::zone_units::EnrichZoneOrder;

use crate::baseline::{AdoptedBaseline, RunKind, ensure_incremental_may_proceed, group_run_kind};
use crate::config::{EnrichmentModeConfig, ProducerConfig};
use crate::cursors::{
    FetchCursorCoordinate, IngestJournalCoordinate, PackageHighWaterMark, RunCheckpoint, RunPhase,
};
use crate::error::ProducerError;
use crate::fetch::{fetch_source, read_fetch_cursor};
use crate::lock::acquire_update_lock;
use crate::runrecord::{RunKindTag, RunRecord};
use crate::timestamp::now_rfc3339;

/// The config value selecting automatic vs manual baseline adoption.
const AUTO_REBASELINE_MODE: &str = "auto-on-new-baseline";

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
    /// FORCE the rebaseline (discard-and-rebuild) path for this run regardless of `[baseline_refresh].mode`
    /// or whether a NEWER DILA baseline is pending — the operator `rebaseline` repair command sets this.
    /// It re-anchors `core` to each source's CURRENTLY-fetched baseline and re-records per-source adoption.
    /// It does NOT invent a second rebaseline mechanism: it drives the SAME [`run_rebaseline_cycle`] /
    /// `rebaseline_cycle` discard-and-rebuild path, under the SAME `update-core` lock, with the SAME
    /// integrity/order/convergence checks and the SAME structured run record as the automatic path.
    pub force_rebaseline: bool,
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
            force_rebaseline: false,
        }
    }

    /// A FORCED-rebaseline repair run for `group` (the operator `rebaseline` command). Equivalent to
    /// [`UpdateOptions::new`] with [`UpdateOptions::force_rebaseline`] set.
    #[must_use]
    pub fn rebaseline(group: impl Into<String>) -> Self {
        let mut options = Self::new(group);
        options.force_rebaseline = true;
        options
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
    /// True when this run drove the REBASELINE path (adopted a newer DILA baseline) rather than an
    /// ordinary incremental.
    pub rebaselined: bool,
    /// The DILA baseline file name(s) adopted this run (rebaseline path only).
    pub adopted_baselines: Vec<String>,
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

/// Classify a completed `rebaseline_cycle` outcome. A rebaseline ALWAYS publishes a package (it is a
/// full re-anchor), so it is `rebaselined`, degraded to `published-enrich-degraded` only if enrichment
/// was skipped for lack of credentials.
#[must_use]
fn classify_rebaseline(report: &RebaselineCycleReport) -> &'static str {
    match &report.enrichment {
        EnrichmentMode::SkippedNoCredentials => "published-enrich-degraded",
        _ => "rebaselined",
    }
}

/// Run the full `update` orchestration for a fetch group, writing a durable [`RunRecord`] at the start
/// AND end (success OR failure) so the outcome is always observable without logs. See the module docs
/// for the lock/ordering contract; see [`crate::baseline`] for the automatic-rebaseline routing.
pub fn run_update(
    config: &ProducerConfig,
    options: &UpdateOptions,
) -> Result<UpdateReport, ProducerError> {
    let sources = config.resolve_group(&options.group)?;
    let source_tokens: Vec<String> = sources.iter().map(|s| s.as_str().to_owned()).collect();
    let run_id = make_run_id(&options.group);
    let state_dir = &config.producer.state_dir;

    let initial_kind = if options.dry_run {
        RunKindTag::DryRun
    } else {
        RunKindTag::Incremental
    };
    let mut record = RunRecord::started(&options.group, &run_id, &source_tokens, initial_kind);
    record.save(state_dir)?;

    match run_update_inner(config, options, &sources, &run_id, &mut record) {
        Ok(report) => {
            record.fetch_cursors = report.fetch_cursors.clone();
            record.ingest_journals = report.ingest_journals.clone();
            record.package_high_water_mark = report.package_high_water_mark.clone();
            record.published_package = report.built_incremental.clone();
            record.adopted_baselines = report.adopted_baselines.clone();
            record.finish(report.exit_class, None);
            record.save(state_dir)?;
            Ok(report)
        }
        Err(err) => {
            // Durable failure record (best-effort save; the original error is what we return).
            record.finish(err.class(), Some(err.to_string()));
            let _ = record.save(state_dir);
            Err(err)
        }
    }
}

/// A unique run id, `<group>-<unix_secs>-<nanos>-<pid>`. The nanosecond fraction AND the pid keep a
/// manual run and a timer run for the SAME group started in the SAME second from colliding on the run
/// record path + `last.json` (which would overwrite one run's observability). The leading
/// `<group>-<unix_secs>` keeps ids human-sortable by start time.
fn make_run_id(group: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "{group}-{}-{:09}-{}",
        now.as_secs(),
        now.subsec_nanos(),
        std::process::id(),
    )
}

/// The inner orchestration (its `Result` is wrapped by [`run_update`] to write the terminal record).
fn run_update_inner(
    config: &ProducerConfig,
    options: &UpdateOptions,
    sources: &[ArchiveSource],
    run_id: &str,
    record: &mut RunRecord,
) -> Result<UpdateReport, ProducerError> {
    let state_dir = &config.producer.state_dir;
    let mut checkpoint = RunCheckpoint::started(&options.group, run_id);
    checkpoint.save(state_dir)?;

    // --- Phase 1: fetch (network only, NO DB writes → runs OUTSIDE the update lock). ---
    let mut fetch_cursors = Vec::new();
    for &source in sources {
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

    // A dry run stops here: it never opens the DB, takes no lock, and publishes nothing. A FORCED
    // rebaseline dry run additionally REPORTS the per-source baselines it WOULD re-anchor + re-adopt
    // (read-only, from the fetch cursors) so the operator can preview the repair without mutating.
    if options.dry_run {
        let planned = if options.force_rebaseline {
            forced_rebaseline_baselines(state_dir, sources)?
        } else {
            Vec::new()
        };
        return Ok(UpdateReport {
            group: options.group.clone(),
            run_id: run_id.to_owned(),
            sources: sources.iter().map(|s| s.as_str().to_owned()).collect(),
            dry_run: true,
            fetch_cursors,
            ingest_journals: Vec::new(),
            enrichment: EnrichmentMode::Disabled,
            built_incremental: None,
            package_high_water_mark: None,
            rebaselined: options.force_rebaseline,
            // In a dry run NOTHING is adopted; this lists the baselines a real run WOULD adopt.
            adopted_baselines: planned.into_iter().map(|(_, name)| name).collect(),
            exit_class: "dry-run",
        });
    }

    // Fail fast (no lock, no DB connection) when a FORCED rebaseline has nothing on disk to re-anchor:
    // no source in the group has a fetched + integrity-checked baseline yet. The authoritative forced set
    // is still recomputed under the lock below; this is only a cheap read-only precondition check.
    if options.force_rebaseline && forced_rebaseline_baselines(state_dir, sources)?.is_empty() {
        return Err(ProducerError::NothingToRebaseline {
            group: options.group.clone(),
        });
    }

    // --- Acquire the single core update lock for the entire DB-mutating span. ---
    let _lock = acquire_update_lock(state_dir, options.lock_wait)?;
    let db = config.writer_handle()?;
    ensure_provisioned(&db)?;

    // --- Routing, RECOMPUTED UNDER THE LOCK (Phase 5, automatic rebaseline): did the fetch reveal a
    //     NEWER DILA baseline still PENDING adoption? Two group timers can both observe a pending
    //     baseline BEFORE either holds the lock, so deciding pre-lock would let both run a rebaseline for
    //     the same upstream baseline. Deciding HERE, from the adoption markers as they stand UNDER the
    //     lock, means a baseline already adopted by the other run (its marker is written post-publish,
    //     under this same lock) is now seen as adopted: this run becomes an ordinary incremental and
    //     never builds a duplicate rebaseline. ---
    let auto = config.baseline_refresh.mode == AUTO_REBASELINE_MODE;
    // FORCED repair (operator `rebaseline`): re-anchor to each source's currently-fetched baseline,
    // bypassing the mode gate and the "is a NEWER baseline pending?" detection (the operator has decided
    // to rebuild). It still drives the SAME discard-and-rebuild path below — it does NOT skip any
    // integrity/order/convergence check, all of which live inside ingest + `rebaseline_cycle`.
    let new_baselines = if options.force_rebaseline {
        let forced = forced_rebaseline_baselines(state_dir, sources)?;
        if forced.is_empty() {
            // Nothing on disk to re-anchor: no source in the group has a fetched+verified baseline yet.
            return Err(ProducerError::NothingToRebaseline {
                group: options.group.clone(),
            });
        }
        forced
    } else {
        match group_run_kind(state_dir, sources)? {
            RunKind::Rebaseline {
                sources_with_new_baseline,
            } => sources_with_new_baseline,
            RunKind::Incremental => Vec::new(),
        }
    };
    if !options.force_rebaseline && !new_baselines.is_empty() && !auto {
        // `manual` mode: REFUSE rather than cross the baseline boundary as a delta (an operator runs the
        // manual rebaseline repair command — M7). This is the hard backstop.
        ensure_incremental_may_proceed(state_dir, sources)?;
    }
    let do_rebaseline = options.force_rebaseline || (!new_baselines.is_empty() && auto);
    if do_rebaseline {
        record.kind = RunKindTag::Rebaseline;
        record.save(state_dir)?;
    }

    // --- Phase 2/3: ingest each source's newly-mirrored archives (baseline precedence is in the
    //     planner; a rebaseline run re-ingests the new global baseline). ---
    let mut ingest_journals = Vec::new();
    for &source in sources {
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
            enrich_group(config, &db, sources)?
        };
    checkpoint.phase = RunPhase::Enriched;
    checkpoint.save(state_dir)?;

    // --- Phase 5: embed pending documents + zone units (document embedding over public text). ---
    embed_pending(config, &db, EmbedTarget::Chunks)?;
    embed_pending(config, &db, EmbedTarget::ZoneUnits)?;
    checkpoint.phase = RunPhase::Embedded;
    checkpoint.save(state_dir)?;

    // --- Phase 6: publish. Either the ordinary incremental cycle OR the rebaseline cycle. ---
    if do_rebaseline {
        let report = run_rebaseline_cycle(config, &db, run_id, enrichment, &new_baselines)?;
        let hwm = PackageHighWaterMark {
            corpus: config.package.corpus.clone(),
            head_sequence: report.head_sequence,
            included_change_seq_high: report.included_change_seq_high,
        };
        checkpoint.phase = RunPhase::Published;
        checkpoint.package_high_water_mark = Some(hwm.clone());
        checkpoint.save(state_dir)?;

        // Record adoption ONLY now — after the signed rebaseline package is published (never before) —
        // PER SOURCE, for EVERY source in this run's `new_baselines`. Under the M3 r3 design the rebaseline
        // is DISCARD-AND-REBUILT from the current locked DB state, so the published artifact's baseline set
        // ALWAYS equals the current run's pending per-source baselines: there is no stale-resume identity
        // to reconcile, and adoption is exact per-source (no max-timestamp-equality heuristic that could
        // adopt a source whose later baseline the published snapshot predated, Codex r3 BLOCKER 1).
        let adopted = adopt_new_baselines(state_dir, &new_baselines)?;
        let exit_class = classify_rebaseline(&report);
        return Ok(UpdateReport {
            group: options.group.clone(),
            run_id: run_id.to_owned(),
            sources: sources.iter().map(|s| s.as_str().to_owned()).collect(),
            dry_run: false,
            fetch_cursors,
            ingest_journals,
            enrichment: report.enrichment,
            built_incremental: Some(report.package_id),
            package_high_water_mark: Some(hwm),
            rebaselined: true,
            adopted_baselines: adopted,
            exit_class,
        });
    }

    // Backstop: an ordinary incremental must never cross a pending baseline boundary.
    ensure_incremental_may_proceed(state_dir, sources)?;
    let cycle = run_cycle(config, &db, run_id, enrichment.clone())?;
    // Record the REAL package coordinates the cycle published: the published head sequence and its frozen
    // `change_seq` window-high. For an empty outbox these reflect the current published head.
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
        run_id: run_id.to_owned(),
        sources: sources.iter().map(|s| s.as_str().to_owned()).collect(),
        dry_run: false,
        fetch_cursors,
        ingest_journals,
        enrichment: cycle.enrichment,
        built_incremental: cycle.built_incremental,
        package_high_water_mark: Some(hwm),
        rebaselined: false,
        adopted_baselines: Vec::new(),
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

/// Run one REBASELINE cycle: build the full-snapshot `Rebaseline` from the producer's current tables
/// (the new DILA baseline has just been ingested), publish it, and refresh the signed manifest. The
/// `baseline_id` is derived from the newest adopted DILA baseline file name so the manifest's
/// `active_baseline` reflects the upstream re-issue.
fn run_rebaseline_cycle(
    config: &ProducerConfig,
    db: &impl DbClientSource,
    run_id: &str,
    enrichment: EnrichmentMode,
    new_baselines: &[(String, String)],
) -> Result<RebaselineCycleReport, ProducerError> {
    let signer = config.signer()?;
    let baseline_id = rebaseline_baseline_id(config, new_baselines);
    let cycle_config = RebaselineCycleConfig {
        baseline_params: rebaseline_params(config, run_id, baseline_id),
        remote_manifest_params: remote_manifest_params(config, &signer),
        enrichment,
    };
    let report = rebaseline_cycle(
        db,
        &config.package.corpus,
        &config.producer.corpora_dir,
        &signer,
        &cycle_config,
    )?;
    Ok(report)
}

/// A deterministic `baseline_id` for a rebaseline, derived from the newest new DILA baseline file name's
/// archive timestamp (e.g. `core-20250713-140000`). Falls back to `<corpus>-rebaseline` if no name parses.
fn rebaseline_baseline_id(config: &ProducerConfig, new_baselines: &[(String, String)]) -> String {
    rebaseline_baseline_id_for(&config.package.corpus, new_baselines)
}

/// The pure `corpus`-keyed core of [`rebaseline_baseline_id`] (extracted so it is unit-testable without a
/// full [`ProducerConfig`]). This LABELS the freshly built rebaseline's embedded `baseline_id` (the
/// manifest's `active_baseline.baseline_id`) from the newest pending baseline's archive timestamp; the
/// label is presentational — adoption is per-source over `new_baselines`, not gated on this id.
fn rebaseline_baseline_id_for(corpus: &str, new_baselines: &[(String, String)]) -> String {
    new_baselines
        .iter()
        .filter_map(|(token, file_name)| {
            let source = ArchiveSource::from_token(token)?;
            ParsedArchive::parse_file_name(source, file_name).ok()
        })
        .map(|parsed| parsed.timestamp.compact().to_owned())
        .max()
        .map(|compact| format!("{corpus}-{compact}"))
        .unwrap_or_else(|| format!("{corpus}-rebaseline"))
}

/// Mark EVERY source in this run's `new_baselines` ADOPTED, PER SOURCE, returning the adopted baseline
/// file names. Called ONLY after the rebaseline package is published. Under the M3 r3 discard-and-rebuild
/// design the published artifact is a full snapshot of the current locked DB state, so it ALWAYS
/// incorporates exactly the current pending per-source baselines — every one is adopted, and none falsely
/// (there is no stale-resume window in which a source's later baseline could be adopted off an older
/// snapshot, Codex r3 BLOCKER 1). Per-source markers mean no source is ever collapsed into another.
fn adopt_new_baselines(
    state_dir: &Path,
    new_baselines: &[(String, String)],
) -> Result<Vec<String>, ProducerError> {
    let mut adopted = Vec::new();
    for (token, baseline) in new_baselines {
        if let Some(source) = ArchiveSource::from_token(token) {
            AdoptedBaseline::adopt(state_dir, source, baseline)?;
            adopted.push(baseline.clone());
        }
    }
    Ok(adopted)
}

/// The per-source baselines a FORCED (operator repair) rebaseline would re-anchor + re-adopt: for EVERY
/// source in the group that has a fetched + integrity-checked baseline on disk (the fetch cursor's
/// `baseline_file_name`), the `(source_token, baseline_file_name)` pair — WHETHER OR NOT it is already
/// adopted. Unlike the automatic path (which acts only on a NEWER pending baseline), a repair re-anchors
/// the whole `core` corpus to the current DILA baseline set regardless. Sources with no fetched baseline
/// yet contribute nothing (there is nothing to re-anchor them to). Read-only: no DB, no network, no lock.
fn forced_rebaseline_baselines(
    state_dir: &Path,
    sources: &[ArchiveSource],
) -> Result<Vec<(String, String)>, ProducerError> {
    let mut forced = Vec::new();
    for &source in sources {
        let cursor = FetchCursor::load(state_dir, source)?;
        if let Some(baseline) = cursor.baseline_file_name {
            forced.push((source.as_str().to_owned(), baseline));
        }
    }
    Ok(forced)
}

/// The preview of what an operator `rebaseline` repair run for `group` WOULD do, computed purely from the
/// on-disk fetch cursors (no DB, no network, no lock, no mutation). Drives the `--dry-run` report and is
/// the planning seam tests assert against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedRebaselinePlan {
    pub group: String,
    /// The group's ordered DILA sources.
    pub sources: Vec<String>,
    /// `(source_token, fetched_baseline_file_name)` the rebaseline would re-anchor + re-adopt. A source
    /// with no fetched baseline yet is absent here.
    pub baselines: Vec<(String, String)>,
}

impl ForcedRebaselinePlan {
    /// True when at least one source has a fetched baseline to re-anchor to (a real run would publish).
    #[must_use]
    pub fn has_work(&self) -> bool {
        !self.baselines.is_empty()
    }
}

/// Plan a FORCED rebaseline for the named fetch `group` from the on-disk fetch cursors. Read-only — it
/// neither fetches, locks, nor mutates; it reports the per-source baselines a real `rebaseline` run would
/// re-anchor + re-adopt through the SAME discard-and-rebuild `rebaseline_cycle` path as the automatic run.
pub fn plan_forced_rebaseline(
    config: &ProducerConfig,
    group: &str,
) -> Result<ForcedRebaselinePlan, ProducerError> {
    let sources = config.resolve_group(group)?;
    let baselines = forced_rebaseline_baselines(&config.producer.state_dir, &sources)?;
    Ok(ForcedRebaselinePlan {
        group: group.to_owned(),
        sources: sources.iter().map(|s| s.as_str().to_owned()).collect(),
        baselines,
    })
}

/// The rebaseline (full-snapshot) build params: the same storage fingerprint + builder versions as an
/// incremental, plus the upstream-derived `baseline_id`. (A rebaseline intentionally MAY supersede a
/// changed fingerprint/builder set, unlike an incremental.)
fn rebaseline_params(config: &ProducerConfig, run_id: &str, baseline_id: String) -> BaselineParams {
    let embedding = config.embedding_config();
    let mut builder_versions = BTreeMap::new();
    builder_versions.insert(
        "jurisearch-producer".to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    BaselineParams {
        baseline_id,
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

#[cfg(test)]
mod tests {
    use jurisearch_fetch::ArchiveSource;

    use super::{adopt_new_baselines, make_run_id, rebaseline_baseline_id_for};
    use crate::baseline::AdoptedBaseline;

    fn baseline_name(src: &str, compact: &str) -> (String, String) {
        (
            src.to_owned(),
            format!("Freemium_{src}_global_{compact}.tar.gz"),
        )
    }

    #[test]
    fn the_rebaseline_baseline_id_labels_from_the_newest_pending_baseline() {
        // The presentational label is the corpus + the MAX archive timestamp across the pending baselines.
        let new_baselines = vec![
            baseline_name("cass", "20250713-140000"),
            baseline_name("inca", "20250712-140000"),
        ];
        assert_eq!(
            rebaseline_baseline_id_for("core", &new_baselines),
            "core-20250713140000",
            "label tracks the newest pending baseline timestamp"
        );
    }

    #[test]
    fn rebaseline_adopts_every_source_per_source_with_no_collapse() {
        // M3 r3 multi-source acceptance: a `jurisprudence`-shaped run with TWO sources pending at
        // DIFFERENT baselines (cass newer, inca older) must adopt BOTH, per source — neither dropped nor
        // collapsed into the other. (The discard-and-rebuild rebaseline always snapshots both, so both
        // are safe to adopt after publish.)
        let state_dir = tempfile::tempdir().expect("tempdir");
        let new_baselines = vec![
            baseline_name("cass", "20250713-140000"),
            baseline_name("inca", "20250712-140000"),
        ];
        let adopted = adopt_new_baselines(state_dir.path(), &new_baselines).expect("adopt");
        assert_eq!(adopted.len(), 2, "both sources adopted");

        let cass =
            AdoptedBaseline::load(state_dir.path(), ArchiveSource::Cass).expect("cass marker");
        let inca =
            AdoptedBaseline::load(state_dir.path(), ArchiveSource::Inca).expect("inca marker");
        assert_eq!(
            cass.baseline_file_name.as_deref(),
            Some("Freemium_cass_global_20250713-140000.tar.gz"),
            "cass adopted its own baseline"
        );
        assert_eq!(
            inca.baseline_file_name.as_deref(),
            Some("Freemium_inca_global_20250712-140000.tar.gz"),
            "inca adopted its own (distinct) baseline — not collapsed to cass's"
        );
    }

    #[test]
    fn run_ids_for_the_same_group_in_the_same_second_are_unique() {
        // A manual + timer run for one group can start in the same second; their run ids (and thus the
        // run-record path + `last.json`) must still differ so neither overwrites the other's record.
        let a = make_run_id("jurisprudence");
        let b = make_run_id("jurisprudence");
        assert_ne!(a, b, "two immediate run ids for the same group must differ");
        assert!(a.starts_with("jurisprudence-"));
        assert!(b.starts_with("jurisprudence-"));
    }
}
