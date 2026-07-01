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
    rebaseline_cycle, rebaseline_preflight,
};
use jurisearch_pipeline::embedding::EmbeddingPoolEndpoint;
use jurisearch_pipeline::{
    ArchiveSyncFilter, BuildZoneUnitsOutcome, EmbedRequest, EmbedTarget, EnrichRequest,
    IngestArchivesRequest, embed_documents, enrich_zones, ingest_archives,
};
use jurisearch_storage::backend::{DbClientSource, WriterHandle};
use jurisearch_storage::ingest_accounting::{
    IngestRunInput, IngestRunStatus, finish_ingest_run_with_client,
    latest_completed_ingest_archive_compact_with_client, start_ingest_run_with_client,
    update_ingest_run_manifest_with_client,
};
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
use crate::timestamp::{
    compact_from_unix, now_rfc3339, now_unix, unix_from_compact_archive_timestamp,
};

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
    /// SNAPSHOT-ONLY (`--from-db`) mode: re-publish the CURRENT producer `public.*` as a fresh full
    /// rebaseline generation by running ONLY the rebaseline publish — SKIPPING fetch (read cursors only),
    /// ingest, enrich, and embed. A pure, mirror-independent DB snapshot: no re-projection, no data
    /// regression. Implies (and is only ever set with) [`UpdateOptions::force_rebaseline`]. It derives the
    /// rebaseline baseline set from the fetch cursor WHEN PRESENT, else the adopted-baseline marker; runs
    /// the media coverage preflight before publishing (fail closed on an under-embedded corpus); and,
    /// AFTER a successful publish, seeds each source's completed-ingest cursor to "now" so the stale-cursor
    /// guard stops firing and normal delta-only timers resume from the operator-accepted gap anchor.
    pub snapshot_only: bool,
    /// PRODUCER-ONLY one-shot override for the stale completed-ingest-cursor guard: when set, a cursor
    /// older than [`STALE_CURSOR_MAX_DAYS`] no longer fails closed with [`ProducerError::IngestCursorStale`]
    /// but resolves the SAME delta-only walk a fresh cursor would (ingest the already-present, contiguous
    /// on-disk deltas from the operator-verified anchor). It overrides ONLY the AGE check — a MALFORMED /
    /// unparseable cursor still fails closed. Set exclusively by the `update` CLI subcommand's
    /// `--accept-stale-cursor`; every other construction path leaves it `false`.
    pub accept_stale_cursor: bool,
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
            snapshot_only: false,
            accept_stale_cursor: false,
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

    /// A SNAPSHOT-ONLY (`--from-db`) rebaseline repair run for `group`: re-publish the CURRENT producer
    /// `public.*` as a fresh full rebaseline generation, skipping fetch/ingest/enrich/embed. Equivalent to
    /// [`UpdateOptions::rebaseline`] (it drives the SAME forced discard-and-rebuild path) plus
    /// [`UpdateOptions::snapshot_only`] set, which additionally skips the pipeline phases, runs the media
    /// coverage preflight, and seeds the completed-ingest cursors after publish.
    #[must_use]
    pub fn rebaseline_from_db(group: impl Into<String>) -> Self {
        let mut options = Self::rebaseline(group);
        options.snapshot_only = true;
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
    /// The zone-unit derivation outcome for this cycle (Phase 4.5: derive `zone_units` from the freshly
    /// enriched `decision_zones`), or `None` when derivation did not run (snapshot-only / `--from-db`, a
    /// dry run, or a group with no cass/inca source). See [`derive_zone_units_if_applicable`].
    pub zone_units: Option<BuildZoneUnitsOutcome>,
    /// The package id built this cycle, or `None` if the outbox window was empty (a no-op publish).
    pub built_incremental: Option<String>,
    pub package_high_water_mark: Option<PackageHighWaterMark>,
    /// True when this run drove the REBASELINE path (adopted a newer DILA baseline) rather than an
    /// ordinary incremental.
    pub rebaselined: bool,
    /// The DILA baseline file name(s) adopted this run (rebaseline path only).
    pub adopted_baselines: Vec<String>,
    /// The source tokens whose completed-ingest cursor was SEEDED to "now" after a successful `--from-db`
    /// snapshot-only publish (empty on every other path). Auditable in the run's JSON output.
    pub cursor_seeded: Vec<String>,
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
    // A `--from-db` snapshot-only run is MIRROR-INDEPENDENT: like `skip_fetch`, it only READS the fetch
    // cursors (no network), because the rebaseline snapshot is built from the current DB, not from archives.
    let mut fetch_cursors = Vec::new();
    for &source in sources {
        let report = if options.skip_fetch || options.snapshot_only {
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
            planned_rebaseline_baselines(state_dir, sources, options.snapshot_only)?
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
            zone_units: None,
            built_incremental: None,
            package_high_water_mark: None,
            rebaselined: options.force_rebaseline,
            // In a dry run NOTHING is adopted; this lists the baselines a real run WOULD adopt.
            adopted_baselines: planned.into_iter().map(|(_, name)| name).collect(),
            cursor_seeded: Vec::new(),
            exit_class: "dry-run",
        });
    }

    // Fail fast (no lock, no DB connection) when a FORCED rebaseline has nothing on disk to re-anchor:
    // no source in the group has a fetched + integrity-checked baseline yet. The authoritative forced set
    // is still recomputed under the lock below; this is only a cheap read-only precondition check.
    if options.force_rebaseline
        && planned_rebaseline_baselines(state_dir, sources, options.snapshot_only)?.is_empty()
    {
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
        // A snapshot-only (`--from-db`) run derives its baseline set MIRROR-INDEPENDENTLY (fetch cursor
        // when present, else the adopted marker); an ordinary forced repair uses the fetched baseline.
        let forced = planned_rebaseline_baselines(state_dir, sources, options.snapshot_only)?;
        if forced.is_empty() {
            // Nothing on disk to re-anchor: no source in the group has a fetched+verified baseline yet
            // (nor, for `--from-db`, an adopted-baseline marker to fall back to).
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
    //     planner; a rebaseline run re-ingests the new global baseline). A `--from-db` snapshot-only run
    //     SKIPS the ingest loop entirely: it is a pure DB snapshot with NO re-projection, so
    //     `ingest_journals` stays empty (and `any_full_scan` below is trivially false / unused). ---
    let ingest_journals = if options.snapshot_only {
        Vec::new()
    } else {
        let ingest_now_unix = now_unix();
        let mut journals = Vec::new();
        for &source in sources {
            let journal = ingest_one(
                config,
                &db,
                source,
                &new_baselines,
                ingest_now_unix,
                options.accept_stale_cursor,
            )?;
            journals.push(journal);
        }
        journals
    };
    checkpoint.phase = RunPhase::Ingested;
    checkpoint.ingest_journals = ingest_journals.clone();
    checkpoint.save(state_dir)?;

    // Cycle-level replay-snapshot policy for the chunk-embed pass: refresh iff ANY source full-scanned
    // this cycle. This is the correct axis (full-scan vs delta-only), NOT "did embed insert rows": a
    // full-scan cycle must leave fresh replay evidence even with few/no new chunks, and a delta-only
    // cycle must skip even if it embedded many new chunks. (Unused on the snapshot-only branch, which
    // never embeds — `ingest_journals` is empty there so this is `false`.)
    let any_full_scan = ingest_journals.iter().any(|journal| journal.full_scan);

    // --- Phase 4: enrich (Judilibre covers cass/inca). Honest skip with no creds. A snapshot-only run
    //     FORCES `EnrichmentMode::Disabled` and never calls `enrich_group` (no re-projection). ---
    let enrichment = if options.snapshot_only
        || options.skip_enrich
        || config.enrichment.mode == EnrichmentModeConfig::Disabled
    {
        EnrichmentMode::Disabled
    } else {
        enrich_group(config, &db, sources)?
    };
    checkpoint.phase = RunPhase::Enriched;
    checkpoint.save(state_dir)?;

    // --- Phase 4.5: derive `zone_units` from the freshly enriched `decision_zones` (motivations/moyens/
    //     dispositif fragments), BEFORE embedding so Phase 5 embeds the derived units and Phase 6 publishes
    //     them in the same window. Runs iff the group has a Judilibre source (cass/inca) and the run is not
    //     snapshot-only. NOT gated on `--skip-enrich`: derivation is deterministic and must reconcile
    //     already-cached zones even when Judilibre is skipped. Keeps `RunPhase::Enriched` (idempotent, no
    //     new checkpoint phase). ---
    let zone_units = derive_zone_units_if_applicable(&db, sources, options.snapshot_only)?;

    // --- Phase 5: embed pending documents + zone units (document embedding over public text). SKIPPED
    //     for a snapshot-only run — the DB is published AS-IS, so the media coverage preflight (below,
    //     in `run_rebaseline_cycle`) is what guarantees the snapshot is fully embedded. ---
    if !options.snapshot_only {
        embed_pending(config, &db, EmbedTarget::Chunks, any_full_scan)?;
        // Uniform API: zone embedding never refreshes the replay snapshot (`embed_zone_units_inner`
        // returns `replay_snapshot: None`), so `any_full_scan` is a no-op here — passed only to keep the
        // call sites uniform.
        embed_pending(config, &db, EmbedTarget::ZoneUnits, any_full_scan)?;
    }
    checkpoint.phase = RunPhase::Embedded;
    checkpoint.save(state_dir)?;

    // --- Phase 6: publish. Either the ordinary incremental cycle OR the rebaseline cycle. ---
    if do_rebaseline {
        // A snapshot-only (`--from-db`) run runs the media coverage preflight BEFORE building the
        // rebaseline (fail closed on an under-embedded / fingerprint-inconsistent corpus, since it
        // deliberately skipped the embed passes above). An ordinary forced/automatic rebaseline just
        // re-projected + re-embedded, so it does not re-run the preflight.
        let report = run_rebaseline_cycle(
            config,
            &db,
            run_id,
            enrichment,
            &new_baselines,
            options.snapshot_only,
        )?;
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

        // Cursor seed (ADJUSTMENTS 2 & 3): ONLY on the `--from-db` path, and ONLY here — AFTER the
        // rebaseline package is published AND the per-source adoption markers are written, still under the
        // update-core lock. Seeding each source's completed-ingest cursor to "now" stops the stale-cursor
        // guard from firing so the next timer resolves delta-only from this operator-accepted gap anchor.
        // It is ingest-cursor-only: the fetch cursor is deliberately left untouched (ADJUSTMENT 3). If the
        // publish above had failed, control never reaches here, so a seed is never written before a publish.
        let cursor_seeded = if options.snapshot_only {
            seed_ingest_cursors(&db, sources)?
        } else {
            Vec::new()
        };

        let exit_class = classify_rebaseline(&report);
        return Ok(UpdateReport {
            group: options.group.clone(),
            run_id: run_id.to_owned(),
            sources: sources.iter().map(|s| s.as_str().to_owned()).collect(),
            dry_run: false,
            fetch_cursors,
            ingest_journals,
            enrichment: report.enrichment,
            zone_units,
            built_incremental: Some(report.package_id),
            package_high_water_mark: Some(hwm),
            rebaselined: true,
            adopted_baselines: adopted,
            cursor_seeded,
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
        zone_units,
        built_incremental: cycle.built_incremental,
        package_high_water_mark: Some(hwm),
        rebaselined: false,
        adopted_baselines: Vec::new(),
        cursor_seeded: Vec::new(),
        exit_class,
    })
}

/// Fail with a clear `producer-db-unprovisioned` diagnostic when the external DB has no schema yet,
/// instead of a raw SQL error deep in ingest.
pub(crate) fn ensure_provisioned(db: &WriterHandle) -> Result<(), ProducerError> {
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

/// The maximum age (days) of a completed-run ingest cursor before a delta-only run REFUSES to
/// delta-skip. DILA keeps deltas server-side for ~62 days; 45d is a conservative margin below that, so a
/// cursor still inside this window means the local delta chain has not aged off the server. Beyond it,
/// fetching may have stopped long enough that intervening deltas are gone — fail closed instead of
/// silently skipping a possible gap.
const STALE_CURSOR_MAX_DAYS: u64 = 45;

/// The archive-selection mode `choose_ingest_mode` resolves for one source: a full baseline+delta walk
/// (`incremental = false`) or a delta-only walk at/after `since_compact` (`incremental = true`). Owned
/// `since_compact` so it outlives the borrowed [`ArchiveSyncFilter`] built from it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ArchiveModeChoice {
    incremental: bool,
    since_compact: Option<String>,
}

/// PURE, per-source ingest-mode decision (extracted so it is unit-testable without a DB). A source with
/// a pending NEW baseline in `new_baselines` full-scans (re-anchor to the new baseline); otherwise the
/// completed-run cursor drives a delta-only walk — falling back to a full walk when there is no cursor
/// (cold DB / hand-loaded corpus), and failing closed with [`ProducerError::IngestCursorStale`] when the
/// cursor is too old to trust the local delta chain.
///
/// When `accept_stale_cursor` is set (the operator `--accept-stale-cursor` one-shot override) an
/// age-stale cursor no longer fails closed — it resolves the SAME delta-only walk a fresh cursor would.
/// The override bypasses ONLY the age check: an unparseable cursor still fails closed regardless.
///
/// # Errors
/// [`ProducerError::IngestCursorStale`] if a present cursor is older than [`STALE_CURSOR_MAX_DAYS`]
/// (unless `accept_stale_cursor` overrides the age check) or is unparseable (always fail-closed-stale).
fn choose_ingest_mode(
    source: ArchiveSource,
    new_baselines: &[(String, String)],
    cursor: Option<&str>,
    now_unix: u64,
    accept_stale_cursor: bool,
) -> Result<ArchiveModeChoice, ProducerError> {
    let full_scan = new_baselines
        .iter()
        .any(|(token, _)| token == source.as_str());
    if full_scan {
        // A pending new baseline: re-walk baseline + all deltas (as before this change).
        return Ok(ArchiveModeChoice {
            incremental: false,
            since_compact: None,
        });
    }
    match cursor {
        // Cold DB / hand-loaded corpus with no completed ingest run: fall back to a full walk.
        None => Ok(ArchiveModeChoice {
            incremental: false,
            since_compact: None,
        }),
        Some(compact) => {
            // A cursor we cannot age is treated as stale (fail closed) rather than delta-skipped. The
            // `--accept-stale-cursor` override bypasses ONLY the AGE staleness (ingest the operator-
            // verified, contiguous on-disk deltas from the anchor) — an unparseable cursor still fails
            // closed regardless of the flag.
            let stale = match unix_from_compact_archive_timestamp(compact) {
                Some(cursor_unix) => {
                    now_unix.saturating_sub(cursor_unix) > STALE_CURSOR_MAX_DAYS * 86_400
                        && !accept_stale_cursor
                }
                None => true,
            };
            if stale {
                Err(ProducerError::IngestCursorStale {
                    source_token: source.as_str().to_owned(),
                    cursor: compact.to_owned(),
                    max_age_days: STALE_CURSOR_MAX_DAYS,
                })
            } else {
                Ok(ArchiveModeChoice {
                    incremental: true,
                    since_compact: Some(compact.to_owned()),
                })
            }
        }
    }
}

/// Ingest one source's mirrored archives. Selection/idempotency is by DILA archive name/timestamp + the
/// per-archive ingest journal — NEVER by package `change_seq`.
///
/// Steady-state runs ingest ONLY new deltas: `choose_ingest_mode` resolves a delta-only walk from the
/// source's completed-run cursor unless a NEW baseline is pending for this source (in `new_baselines`),
/// in which case it full-scans the baseline + all deltas. After ingest, a run that did not reach
/// `Completed` is a HARD FAILURE — the pipeline must NOT advance (enrich/embed/publish) on a partial
/// ingest, and the completed-run cursor must not be corrupted by treating a failed run as progress.
fn ingest_one(
    config: &ProducerConfig,
    db: &impl DbClientSource,
    source: ArchiveSource,
    new_baselines: &[(String, String)],
    now_unix: u64,
    accept_stale_cursor: bool,
) -> Result<IngestJournalCoordinate, ProducerError> {
    let mirror_dir = config.producer.archives_dir.join(source.as_str());
    let quarantine_dir = config.producer.state_dir.join("ingest-quarantine");

    // Resolve the archive-selection mode. A source with a pending NEW baseline full-scans and needs no
    // cursor, so read the DB-authoritative completed-run cursor ONLY for a non-full-scan source: a
    // malformed stored cursor (a `StorageError` from the helper) must never block a full-scan
    // re-anchor/repair that does not depend on it. `choose_ingest_mode` stays pure and re-derives
    // `full_scan` itself, so passing `None` for a full-scan source is correct (it returns full anyway).
    let full_scan = new_baselines
        .iter()
        .any(|(token, _)| token == source.as_str());
    let cursor = if full_scan {
        None
    } else {
        let mut client = db.client()?;
        latest_completed_ingest_archive_compact_with_client(&mut client, source.as_str())?
    };
    let mode = choose_ingest_mode(
        source,
        new_baselines,
        cursor.as_deref(),
        now_unix,
        accept_stale_cursor,
    )?;

    let req = IngestArchivesRequest {
        source,
        archives_dir: &mirror_dir,
        run_id: None,
        limit_members: None,
        max_member_bytes: MAX_MEMBER_BYTES,
        quarantine_dir: Some(&quarantine_dir),
        safe_mode: false,
        // Selection keys on the DILA archive timestamp via the planner + the journal's de-dup of
        // already-processed file names; it does not consult `change_seq`. `incremental = true` skips the
        // baseline tar entirely and selects only deltas with `compact >= since_compact`.
        filter: ArchiveSyncFilter {
            incremental: mode.incremental,
            since_compact: mode.since_compact.as_deref(),
        },
        // Delta-only cycles skip the full-corpus replay-snapshot rehash; full-scan cycles
        // (rebaseline / pending baseline / cold cursor → `incremental=false`) refresh as before. A stale
        // cursor is not a full-scan case — `choose_ingest_mode` fails closed (`IngestCursorStale`) before ingest.
        refresh_replay_snapshot: !mode.incremental,
    };
    let report = ingest_archives(db, req)?;
    // Fail closed on a member-failed (or otherwise non-completed) ingest run: the producer must not
    // publish after a partial ingest, and the next run's delta-only cursor must not advance past an
    // archive whose members did not all reach inserted/skipped.
    if report.run_status != IngestRunStatus::Completed {
        return Err(ProducerError::IngestRunNotCompleted {
            source_token: source.as_str().to_owned(),
            run_id: report.run_id,
            run_status: report.run_status.as_str().to_owned(),
            failed_members: report.failed_members,
        });
    }
    Ok(IngestJournalCoordinate {
        source: source.as_str().to_owned(),
        run_id: Some(report.run_id),
        journal_compact_timestamp: report.journal_cursor,
        archives_ingested: report.archives_ingested,
        // Same signal passed as the ingest `refresh_replay_snapshot`; the cycle-level `any_full_scan`
        // (OR across sources) reads this to gate the chunk-embed replay-snapshot refresh.
        full_scan: !mode.incremental,
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

/// Derive `zone_units` from the freshly enriched `decision_zones` (Phase 4.5), between enrichment and
/// embedding. Runs iff the run is NOT snapshot-only AND the group contains a Judilibre source
/// (`cass`/`inca`) — the only sources that can carry official zones, matching the derivable selector's
/// `source IN ('cass','inca')` scope so legislation-only cycles skip a pointless scan.
///
/// Deliberately NOT gated on `--skip-enrich`: `--skip-enrich` means "do not call Judilibre", not "skip
/// deterministic derivation". A prior interrupted or operator-run enrichment can leave `decision_zones`
/// rows with absent/stale `zone_units`; derivation is idempotent and reconciles them. Because every `ok`
/// row yields >= 1 unit, a derived decision drops out of the derivable set (no repeated outbox churn).
fn derive_zone_units_if_applicable(
    db: &impl DbClientSource,
    sources: &[ArchiveSource],
    snapshot_only: bool,
) -> Result<Option<BuildZoneUnitsOutcome>, ProducerError> {
    let has_judilibre_source = sources
        .iter()
        .any(|s| matches!(s, ArchiveSource::Cass | ArchiveSource::Inca));
    if snapshot_only || !has_judilibre_source {
        return Ok(None);
    }
    let outcome = jurisearch_pipeline::build_zone_units(
        db,
        jurisearch_pipeline::BuildZoneUnitsRequest {
            limit: None,
            rebuild: false,
        },
    )?;
    Ok(Some(outcome))
}

/// Embed the pending set for a target. A "no rows pending" outcome is a NO-OP (an empty run is a
/// success), not a failure — only genuine endpoint/DB/fingerprint failures propagate.
fn embed_pending(
    config: &ProducerConfig,
    db: &impl DbClientSource,
    target: EmbedTarget,
    refresh_replay_snapshot: bool,
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
        // Only the Chunks target consumes this (ZoneUnits never refreshes); the cycle passes the
        // `any_full_scan` signal so a delta-only cycle skips the full-corpus replay-snapshot rehash.
        refresh_replay_snapshot,
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
    snapshot_preflight: bool,
) -> Result<RebaselineCycleReport, ProducerError> {
    let signer = config.signer()?;
    let baseline_id = rebaseline_baseline_id(config, new_baselines);
    let baseline_params = rebaseline_params(config, run_id, baseline_id);
    // Media coverage preflight for the snapshot-only (`--from-db`) path ONLY: this run skipped embed, so
    // assert (fail closed) that EVERY chunk/zone-unit is fully + consistently embedded under the publish
    // fingerprint/model/dimension BEFORE building the rebaseline. Never publish an under-embedded corpus.
    if snapshot_preflight {
        rebaseline_preflight(db, &baseline_params)?;
    }
    let cycle_config = RebaselineCycleConfig {
        baseline_params,
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

/// The per-source baseline set a `--from-db` snapshot-only rebaseline re-anchors to, MIRROR-INDEPENDENTLY
/// (ADJUSTMENT 1): for EACH source, the fetch cursor's `baseline_file_name` WHEN PRESENT, ELSE a fallback
/// to the adopted-baseline marker's `baseline_file_name`. A source with NEITHER contributes nothing (the
/// snapshot still captures its data; adoption is per-source and the group-level empty set is surfaced as
/// [`ProducerError::NothingToRebaseline`] by the caller). This is the crucial difference from
/// [`forced_rebaseline_baselines`], which reads ONLY the fetch cursor: a host whose fetch cursor is empty
/// (or stale) but whose adoption markers are intact — the exact repair target — can still snapshot.
/// Read-only: no DB, no network, no lock.
fn snapshot_only_baselines(
    state_dir: &Path,
    sources: &[ArchiveSource],
) -> Result<Vec<(String, String)>, ProducerError> {
    let mut set = Vec::new();
    for &source in sources {
        let cursor = FetchCursor::load(state_dir, source)?;
        let baseline = match cursor.baseline_file_name {
            Some(name) => Some(name),
            None => AdoptedBaseline::load(state_dir, source)?.baseline_file_name,
        };
        if let Some(name) = baseline {
            set.push((source.as_str().to_owned(), name));
        }
    }
    Ok(set)
}

/// Select the per-source rebaseline baseline set for a forced repair run: the mirror-independent
/// [`snapshot_only_baselines`] for a `--from-db` run, else the fetch-cursor-only
/// [`forced_rebaseline_baselines`]. A single seam so the dry-run preview, the pre-lock precondition, and
/// the under-lock authoritative computation all agree on the source of truth.
fn planned_rebaseline_baselines(
    state_dir: &Path,
    sources: &[ArchiveSource],
    snapshot_only: bool,
) -> Result<Vec<(String, String)>, ProducerError> {
    if snapshot_only {
        snapshot_only_baselines(state_dir, sources)
    } else {
        forced_rebaseline_baselines(state_dir, sources)
    }
}

/// Seed EACH group source's completed-ingest cursor to "now" AFTER a successful `--from-db` snapshot-only
/// publish (ADJUSTMENTS 2 & 3). For every source it inserts ONE synthetic `completed` `ingest_run` via the
/// SAME lifecycle helpers a real ingest uses — `start` → `update manifest` → `finish(Completed)` — whose
/// manifest carries `freshness.latest_archive_timestamp_compact = <now>` and `member_limited = false`, so
/// it advances [`latest_completed_ingest_archive_compact_with_client`] to `<now>`. The next timer then
/// resolves delta-only (`since = now`) instead of failing closed with `IngestCursorStale`; the operator
/// accepts the DILA retention gap. The run is memberless — harmless (the `ingest_member` FK is child-only,
/// and the health view tolerates zero members). It is INGEST-CURSOR-ONLY: it NEVER touches the fetch
/// cursor (ADJUSTMENT 3). Returns the seeded source tokens for the run's JSON output.
fn seed_ingest_cursors(
    db: &impl DbClientSource,
    sources: &[ArchiveSource],
) -> Result<Vec<String>, ProducerError> {
    // One consistent "now": the same unix instant labels the run id AND the compact freshness anchor.
    let seed_unix = now_unix();
    let now_compact = compact_from_unix(seed_unix);
    let schema_version = jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION.to_string();
    let code_version = env!("CARGO_PKG_VERSION");
    let manifest_json = serde_json::json!({
        "freshness": {
            "latest_archive_timestamp_compact": now_compact,
            "member_limited": false,
        },
        "kind": "cursor_seed",
        "note": "operator accepted DILA retention gap; DB-snapshot rebaseline core-2-3 published",
    })
    .to_string();

    let mut client = db.client()?;
    let mut seeded = Vec::new();
    for &source in sources {
        let run_id = format!("cursor-seed-{}-{}", source.as_str(), seed_unix);
        let input = IngestRunInput {
            run_id: &run_id,
            source: source.as_str(),
            parser_version: "cursor-seed",
            schema_version: &schema_version,
            code_version,
            safe_mode: false,
            archive_plan_json: None,
            manifest_json: None,
        };
        start_ingest_run_with_client(&mut client, &input)?;
        update_ingest_run_manifest_with_client(&mut client, &run_id, &manifest_json)?;
        finish_ingest_run_with_client(&mut client, &run_id, IngestRunStatus::Completed, None)?;
        seeded.push(source.as_str().to_owned());
    }
    Ok(seeded)
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

/// Plan a `--from-db` snapshot-only rebaseline for the named `group` from ON-DISK state only, computed
/// MIRROR-INDEPENDENTLY (fetch cursor's `baseline_file_name` when present, else the adopted-baseline
/// marker — ADJUSTMENT 1's snapshot source). Read-only in the strongest sense: it neither fetches nor
/// opens the DB, takes NO lock, and writes NO `state_dir` file (no `RunRecord`, no `RunCheckpoint`), so a
/// `rebaseline --from-db --dry-run` stays side-effect-free — exactly like [`plan_forced_rebaseline`] does
/// for the fetch-cursor-only forced repair. It reports the per-source baseline set a REAL `--from-db` run
/// would re-anchor + re-adopt; a real run would additionally seed each source's completed-ingest cursor.
pub fn plan_snapshot_rebaseline(
    config: &ProducerConfig,
    group: &str,
) -> Result<ForcedRebaselinePlan, ProducerError> {
    let sources = config.resolve_group(group)?;
    let baselines = planned_rebaseline_baselines(&config.producer.state_dir, &sources, true)?;
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
pub(crate) fn remote_manifest_params(
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
    use jurisearch_fetch::{ArchiveSource, FetchCursor};

    use super::{
        ArchiveModeChoice, STALE_CURSOR_MAX_DAYS, UpdateOptions, adopt_new_baselines,
        choose_ingest_mode, make_run_id, planned_rebaseline_baselines, rebaseline_baseline_id_for,
        snapshot_only_baselines,
    };
    use crate::baseline::AdoptedBaseline;
    use crate::cursors::IngestJournalCoordinate;
    use crate::error::ProducerError;
    use crate::timestamp::compact_from_unix;

    // A fixed "now" for cursor-age tests: 2026-06-29T12:00:00Z.
    const NOW_UNIX: u64 = 1_782_734_400;

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
    fn choose_ingest_mode_full_scans_a_source_with_a_pending_new_baseline() {
        // A pending NEW baseline for this source re-anchors the whole corpus: full baseline + deltas,
        // regardless of any cursor.
        let new_baselines = vec![baseline_name("legi", "20260101-000000")];
        let choice = choose_ingest_mode(
            ArchiveSource::Legi,
            &new_baselines,
            Some("20260615120000"),
            NOW_UNIX,
            false,
        )
        .expect("full scan");
        assert_eq!(
            choice,
            ArchiveModeChoice {
                incremental: false,
                since_compact: None,
            }
        );
    }

    #[test]
    fn choose_ingest_mode_full_scans_when_there_is_no_cursor() {
        // Cold DB / hand-loaded corpus with no completed ingest run: fall back to a full walk.
        let choice =
            choose_ingest_mode(ArchiveSource::Legi, &[], None, NOW_UNIX, false).expect("full scan");
        assert_eq!(
            choice,
            ArchiveModeChoice {
                incremental: false,
                since_compact: None,
            }
        );
    }

    #[test]
    fn choose_ingest_mode_delta_only_from_a_fresh_cursor() {
        // No pending baseline + a cursor well inside the retention window: delta-only since the cursor.
        let choice = choose_ingest_mode(
            ArchiveSource::Legi,
            &[],
            Some("20260615120000"),
            NOW_UNIX,
            false,
        )
        .expect("delta only");
        assert_eq!(
            choice,
            ArchiveModeChoice {
                incremental: true,
                since_compact: Some("20260615120000".to_owned()),
            }
        );
    }

    #[test]
    fn choose_ingest_mode_fails_closed_on_a_stale_cursor() {
        // A cursor older than STALE_CURSOR_MAX_DAYS (here ~89 days) must fail closed, never delta-skip.
        let error = choose_ingest_mode(
            ArchiveSource::Legi,
            &[],
            Some("20260401120000"),
            NOW_UNIX,
            false,
        )
        .expect_err("stale cursor");
        match error {
            ProducerError::IngestCursorStale {
                source_token,
                cursor,
                max_age_days,
            } => {
                assert_eq!(source_token, "legi");
                assert_eq!(cursor, "20260401120000");
                assert_eq!(max_age_days, STALE_CURSOR_MAX_DAYS);
            }
            other => panic!("expected IngestCursorStale, got {other:?}"),
        }
    }

    #[test]
    fn choose_ingest_mode_fails_closed_on_an_unparseable_cursor() {
        // A cursor we cannot age (malformed) is treated as stale (fail closed), not delta-skipped.
        let error = choose_ingest_mode(
            ArchiveSource::Legi,
            &[],
            Some("not-a-compact"),
            NOW_UNIX,
            false,
        )
        .expect_err("unparseable cursor");
        assert!(matches!(error, ProducerError::IngestCursorStale { .. }));
    }

    #[test]
    fn accept_stale_cursor_overrides_only_the_age_check() {
        // The `--accept-stale-cursor` one-shot override turns the age-staleness HARD error into the SAME
        // delta-only walk a fresh cursor resolves (ingest the operator-verified, contiguous on-disk deltas
        // from the anchor). It must NOT bypass the parse/validity check: a MALFORMED cursor still fails
        // closed even with the override on.
        let stale = "20260401120000";

        // Age-stale + flag off: still fails closed (unchanged behavior).
        let error = choose_ingest_mode(ArchiveSource::Legi, &[], Some(stale), NOW_UNIX, false)
            .expect_err("stale cursor still fails closed when the flag is off");
        assert!(matches!(error, ProducerError::IngestCursorStale { .. }));

        // Age-stale + flag on: the SAME delta-only choice a fresh cursor would resolve.
        let choice = choose_ingest_mode(ArchiveSource::Legi, &[], Some(stale), NOW_UNIX, true)
            .expect("override resolves delta-only from the accepted anchor");
        assert_eq!(
            choice,
            ArchiveModeChoice {
                incremental: true,
                since_compact: Some(stale.to_owned()),
            }
        );

        // Malformed cursor + flag on: the override bypasses ONLY the age check, so this STILL fails closed.
        let error = choose_ingest_mode(
            ArchiveSource::Legi,
            &[],
            Some("not-a-compact"),
            NOW_UNIX,
            true,
        )
        .expect_err("a malformed cursor fails closed even with the override on");
        assert!(matches!(error, ProducerError::IngestCursorStale { .. }));
    }

    #[test]
    fn choose_ingest_mode_is_per_source_in_a_multi_source_group() {
        // Only the source(s) present in `new_baselines` full-scan; the other source with a fresh cursor
        // still resolves to delta-only.
        let new_baselines = vec![baseline_name("cass", "20260101-000000")];

        let cass = choose_ingest_mode(
            ArchiveSource::Cass,
            &new_baselines,
            Some("20260615120000"),
            NOW_UNIX,
            false,
        )
        .expect("cass full scan");
        assert_eq!(
            cass,
            ArchiveModeChoice {
                incremental: false,
                since_compact: None,
            },
            "cass has a pending baseline -> full scan"
        );

        let inca = choose_ingest_mode(
            ArchiveSource::Inca,
            &new_baselines,
            Some("20260615120000"),
            NOW_UNIX,
            false,
        )
        .expect("inca delta only");
        assert_eq!(
            inca,
            ArchiveModeChoice {
                incremental: true,
                since_compact: Some("20260615120000".to_owned()),
            },
            "inca has no pending baseline -> delta-only from its cursor"
        );
    }

    /// The replay-snapshot refresh policy the producer threads into ingest is exactly `!incremental`
    /// for EVERY mode `choose_ingest_mode` can resolve: full-scan modes (pending baseline / cold cursor)
    /// refresh; a delta-only mode skips. This is the same value `ingest_one` also stores as the journal's
    /// `full_scan`, so the two never diverge.
    #[test]
    fn ingest_refresh_policy_is_the_negation_of_incremental_for_every_mode() {
        // Full-scan: a pending new baseline for this source.
        let new_baselines = vec![baseline_name("legi", "20260101-000000")];
        let full = choose_ingest_mode(ArchiveSource::Legi, &new_baselines, None, NOW_UNIX, false)
            .expect("full scan");
        assert!(
            !full.incremental,
            "full-scan cycle refreshes: !incremental == true"
        );

        // Full-scan: cold DB / hand-loaded corpus, no cursor.
        let cold = choose_ingest_mode(ArchiveSource::Legi, &[], None, NOW_UNIX, false)
            .expect("cold full scan");
        assert!(!cold.incremental);

        // Delta-only: a fresh completed-run cursor, no pending baseline.
        let delta = choose_ingest_mode(
            ArchiveSource::Legi,
            &[],
            Some("20260615120000"),
            NOW_UNIX,
            false,
        )
        .expect("delta only");
        assert!(
            delta.incremental,
            "delta-only cycle skips: !incremental == false"
        );
    }

    fn journal_with_full_scan(source: &str, full_scan: bool) -> IngestJournalCoordinate {
        IngestJournalCoordinate {
            source: source.to_owned(),
            run_id: Some(format!("{source}-run")),
            journal_compact_timestamp: Some("20260628200000".to_owned()),
            archives_ingested: 1,
            full_scan,
        }
    }

    /// The cycle-level `any_full_scan` (the chunk-embed refresh gate) is the OR across per-source
    /// journals: all-delta ⇒ false (skip the embed refresh), any full ⇒ true (refresh).
    #[test]
    fn any_full_scan_is_the_or_across_source_journals() {
        let all_delta = [
            journal_with_full_scan("cass", false),
            journal_with_full_scan("inca", false),
        ];
        assert!(
            !all_delta.iter().any(|journal| journal.full_scan),
            "a delta-only cycle skips the chunk-embed replay refresh"
        );

        let mixed = [
            journal_with_full_scan("cass", false),
            journal_with_full_scan("inca", true),
        ];
        assert!(
            mixed.iter().any(|journal| journal.full_scan),
            "one full-scan source forces the cycle-level refresh"
        );

        // No sources ingested this cycle ⇒ no full scan ⇒ skip.
        let none: Vec<IngestJournalCoordinate> = Vec::new();
        assert!(!none.iter().any(|journal| journal.full_scan));
    }

    #[test]
    fn rebaseline_from_db_forces_a_snapshot_only_repair_run() {
        // `--from-db` drives the SAME forced discard-and-rebuild path (force_rebaseline) but additionally
        // marks the run snapshot-only (skip fetch/ingest/enrich/embed + preflight + cursor-seed). It is
        // NOT a dry run and does not silently skip enrichment via the ordinary knob.
        let options = UpdateOptions::rebaseline_from_db("jurisprudence");
        assert!(
            options.force_rebaseline,
            "from-db still drives the forced rebaseline path"
        );
        assert!(options.snapshot_only, "from-db is snapshot-only");
        assert!(!options.dry_run, "a real repair, not a preview");
        assert!(!options.skip_enrich);
        assert_eq!(options.group, "jurisprudence");
    }

    #[test]
    fn snapshot_only_baselines_falls_back_to_the_adopted_marker_without_a_fetch_cursor() {
        // The repair target: the fetch cursor is EMPTY (mirror-independent host) but the per-source
        // adoption marker still holds the last adopted baseline. The snapshot baseline set (ADJUSTMENT 1)
        // must fall back to the adopted marker rather than treating the group as `NothingToRebaseline`.
        let state_dir = tempfile::tempdir().expect("tempdir");
        AdoptedBaseline::adopt(
            state_dir.path(),
            ArchiveSource::Cass,
            "Freemium_cass_global_20250713-140000.tar.gz",
        )
        .expect("adopt");
        let set = snapshot_only_baselines(state_dir.path(), &[ArchiveSource::Cass]).expect("set");
        assert_eq!(
            set,
            vec![(
                "cass".to_owned(),
                "Freemium_cass_global_20250713-140000.tar.gz".to_owned()
            )],
            "fell back to the adopted baseline when the fetch cursor lacked one"
        );
    }

    #[test]
    fn snapshot_only_baselines_prefers_the_fetch_cursor_over_the_adopted_marker() {
        // When BOTH exist, the fetch cursor's baseline (the newest downloaded + integrity-checked one)
        // takes precedence over the older adopted marker.
        let state_dir = tempfile::tempdir().expect("tempdir");
        AdoptedBaseline::adopt(
            state_dir.path(),
            ArchiveSource::Cass,
            "Freemium_cass_global_20250101-000000.tar.gz",
        )
        .expect("adopt");
        let mut cursor = FetchCursor::new(ArchiveSource::Cass);
        cursor.baseline_file_name = Some("Freemium_cass_global_20250713-140000.tar.gz".to_owned());
        cursor.save(state_dir.path()).expect("save cursor");
        let set = snapshot_only_baselines(state_dir.path(), &[ArchiveSource::Cass]).expect("set");
        assert_eq!(
            set,
            vec![(
                "cass".to_owned(),
                "Freemium_cass_global_20250713-140000.tar.gz".to_owned()
            )],
            "the fetch cursor baseline wins over the adopted marker"
        );
    }

    #[test]
    fn a_seeded_now_cursor_resolves_delta_only_not_stale() {
        // The end-state the `--from-db` cursor seed engineers: with a completed-run cursor at "now" (the
        // seeded compact anchor), the NEXT ingest resolves delta-only from that anchor — NOT
        // `IngestCursorStale`. This closes the loop between `compact_from_unix` (what the seed writes) and
        // `choose_ingest_mode` (what the next timer reads).
        let seed_compact = compact_from_unix(NOW_UNIX);
        let choice = choose_ingest_mode(
            ArchiveSource::Cass,
            &[],
            Some(&seed_compact),
            NOW_UNIX,
            false,
        )
        .expect("delta-only from the freshly seeded anchor");
        assert_eq!(
            choice,
            ArchiveModeChoice {
                incremental: true,
                since_compact: Some(seed_compact),
            },
            "a just-seeded cursor is well inside the stale window -> delta-only since the anchor"
        );
    }

    #[test]
    fn the_snapshot_dry_run_planning_seam_writes_no_state_dir_files() {
        // The `--from-db --dry-run` short-circuit plans from on-disk state via
        // `planned_rebaseline_baselines(.., snapshot_only=true)` WITHOUT any side effect: it only READS
        // the fetch cursor + adoption marker and never writes a RunRecord/RunCheckpoint (or any other
        // state-dir file). Asserted by snapshotting the state dir before/after.
        let state_dir = tempfile::tempdir().expect("tempdir");
        AdoptedBaseline::adopt(
            state_dir.path(),
            ArchiveSource::Cass,
            "Freemium_cass_global_20250713-140000.tar.gz",
        )
        .expect("adopt");
        let entries = || {
            let mut names: Vec<_> = std::fs::read_dir(state_dir.path())
                .expect("read_dir")
                .map(|e| e.expect("entry").file_name())
                .collect();
            names.sort();
            names
        };
        let before = entries();
        let plan = planned_rebaseline_baselines(state_dir.path(), &[ArchiveSource::Cass], true)
            .expect("plan");
        assert_eq!(
            entries(),
            before,
            "planning a --from-db dry run wrote no new state-dir files"
        );
        assert_eq!(
            plan,
            vec![(
                "cass".to_owned(),
                "Freemium_cass_global_20250713-140000.tar.gz".to_owned()
            )],
            "the preview reports the mirror-independent snapshot baseline set"
        );
    }

    #[test]
    fn snapshot_only_baselines_omits_a_source_with_neither_baseline() {
        // A source with no fetch cursor AND no adoption marker contributes nothing; the group-level empty
        // set is what the caller surfaces as `NothingToRebaseline`.
        let state_dir = tempfile::tempdir().expect("tempdir");
        let set = snapshot_only_baselines(
            state_dir.path(),
            &[ArchiveSource::Cass, ArchiveSource::Inca],
        )
        .expect("set");
        assert!(
            set.is_empty(),
            "no fetch cursor and no adopted marker -> nothing to re-anchor"
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
