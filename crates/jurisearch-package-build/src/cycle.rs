//! The operated-producer cycle seam (plan P9): one ingest→build→sign→publish→refresh-manifest pass,
//! callable by tests/CLI now and by a cron/daemon later (the CADENCE is the ops boundary).
//!
//! Proactive enrichment ORCHESTRATION (running the existing `enrich-zones` / `build-zone-units` /
//! `embed-zone-units` steps before packaging) is the CALLER's responsibility — those steps live in the
//! producer CLI, and the enriched `decision_zones` / `zone_units` flow through the outbox into the
//! package automatically. The cycle RECORDS the enrichment outcome the caller supplies so a published
//! manifest never silently claims enrichment that did not run.
//!
//! ## Exactly-once / no-partial publish (the acceptance gate)
//!
//! A built incremental is materialised into the per-corpus STAGING slot
//! ([`staged_pending_dir`]) BEFORE its catalog row exists, so the durable artifact and the `"built"`
//! catalog row are both on disk before any publish is attempted. Publish then copies the staged artifact
//! to the served `packages/<id>` (atomic, idempotent) and only THEN marks the catalog row `"published"`.
//! If a cycle dies anywhere between cataloging and a visible artifact, the next [`producer_cycle`] RESUMES
//! the staged artifact — re-publishing the SAME `package_id` (never building a new one) and marking it
//! published — so the manifest only ever advances over an artifact that exists at the served root.
//!
//! ## INCREMENTAL resume vs REBASELINE discard-and-rebuild (M3 r3 design change)
//!
//! The two paths handle a crashed-mid-publish staging slot DIFFERENTLY, because their identities differ:
//!
//! - an INCREMENTAL is a +1 chain link, so [`producer_cycle`] must RESUME the SAME package (publish-once /
//!   chain integrity). [`resume_pending`] first VERIFIES the staged manifest has a matching `package_catalog`
//!   row: an UNCATALOGED staged artifact is an incomplete build (the crash hit before the catalog insert),
//!   so it is DISCARDED and rebuilt rather than published as a phantom (Codex r3 BLOCKER 2).
//! - a REBASELINE is a FULL SNAPSHOT, so [`rebaseline_cycle`] NEVER resumes a stale staged rebaseline:
//!   rebuilding from the CURRENT locked DB state is always safe and makes the published artifact ALWAYS
//!   equal the current pending baseline set, which removes the adoption-mismatch class entirely. An
//!   incomplete prior rebaseline attempt (cataloged-`"built"` OR uncataloged) is DISCARDED — its staging
//!   slot deleted and its orphaned unpublished catalog row removed so it can never be a chain head or
//!   surface in the manifest — then a fresh rebaseline is built, published, and adopted per-source.

use std::path::Path;
use std::path::PathBuf;

use jurisearch_package::PackageKind;
use jurisearch_package::Signer;
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::signed::Signed;

use jurisearch_storage::backend::DbClientSource;
use jurisearch_storage::package_catalog::{
    catalog_rows_for_corpus, delete_unpublished_package_row, package_catalog_status,
};
use jurisearch_storage::runtime::StorageError;

use crate::baseline::{BaselineParams, RebaselineBuildReport, build_rebaseline};
use crate::error::BuildError;
use crate::incremental::{IncrementalParams, build_incremental};
use crate::publish::{publish_package, publish_remote_manifest, staged_pending_dir};
use crate::remote_manifest::{RemoteManifestParams, build_remote_manifest};

/// The enrichment outcome for this cycle (recorded, not run, by the cycle — see the module docs). The
/// CLI decides fail-closed-vs-skip when credentials are absent; the cycle never fabricates a "ran".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrichmentMode {
    /// Proactive enrichment is off for this profile.
    Disabled,
    /// Enrichment ran before this cycle and refreshed `zones_enriched` decisions.
    Ran { zones_enriched: u64 },
    /// Enrichment was requested but upstream credentials were absent (a local/test profile).
    SkippedNoCredentials,
}

/// Inputs for one [`producer_cycle`] pass.
#[derive(Debug, Clone)]
pub struct ProducerCycleConfig {
    pub incremental_params: IncrementalParams,
    pub remote_manifest_params: RemoteManifestParams,
    pub enrichment: EnrichmentMode,
}

/// What one cycle produced.
#[derive(Debug, Clone)]
pub struct ProducerCycleReport {
    pub corpus: String,
    /// The incremental package id PUBLISHED this cycle (a freshly built one OR a resumed staged one), or
    /// `None` if the outbox window was empty and nothing needed resuming (a no-op).
    pub built_incremental: Option<String>,
    /// The published package-sequence head AFTER this cycle (the newest cataloged+published package), if
    /// any catalog row exists. Feeds the producer's `PackageHighWaterMark` checkpoint.
    pub head_sequence: Option<u64>,
    /// The frozen outbox `change_seq` window-high of the published head, if any. Unchanged across an
    /// empty cycle (it reflects the latest published incremental's window).
    pub included_change_seq_high: Option<u64>,
    pub remote_manifest_path: PathBuf,
    pub enrichment: EnrichmentMode,
}

/// A TEST-ONLY fault seam: where (if anywhere) to inject a simulated publish-phase failure, so a gated
/// test can prove the exactly-once / resumable publish contract without making the filesystem unwritable.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishFault {
    /// Production: no injected fault.
    None,
    /// Fail AFTER a fresh incremental is staged + cataloged (`"built"`) but BEFORE it is published —
    /// reproducing the pre-publish crash window the acceptance gate forbids.
    AfterStageBeforePublish,
}

/// Run one producer cycle: resume any staged-but-unpublished incremental, build the next incremental from
/// the outbox (if any changes), publish it, then rebuild + publish the corpus's signed remote manifest
/// from the published artifacts. The active baseline must already be published (the operator publishes it
/// once via `package publish`). Built incrementals are staged under the served root's `.staging/pending`
/// slot, so a crashed publish is recoverable on the next pass.
///
/// # Errors
/// [`BuildError`] on a build/publish/manifest/DB/IO/signing failure.
pub fn producer_cycle(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    signer: &dyn Signer,
    config: &ProducerCycleConfig,
) -> Result<ProducerCycleReport, BuildError> {
    producer_cycle_faulted(
        producer,
        corpus,
        published_root,
        signer,
        config,
        PublishFault::None,
    )
}

/// [`producer_cycle`] with a test fault seam. Hidden from docs; production callers use [`producer_cycle`].
///
/// # Errors
/// [`BuildError`] on a build/publish/manifest/DB/IO/signing failure (or the injected fault).
#[doc(hidden)]
pub fn producer_cycle_faulted(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    signer: &dyn Signer,
    config: &ProducerCycleConfig,
    fault: PublishFault,
) -> Result<ProducerCycleReport, BuildError> {
    let pending = staged_pending_dir(published_root, corpus);

    // 0. RESUME: a staged artifact left by a prior crashed cycle is published FIRST (idempotently) and
    //    marked, so the chain head is a published artifact before we consider building a new one.
    let resumed = resume_pending(producer, corpus, published_root, &pending)?.map(|r| r.package_id);

    // 1. Build the next incremental DIRECTLY into the durable staging slot (None if the window is empty).
    //    Building under the served root means the artifact is on disk before `build_incremental` inserts
    //    the `"built"` catalog row — closing the catalog-advances-before-artifact-exists window.
    clean_dir(&pending)?;
    std::fs::create_dir_all(&pending)?;
    let incremental = build_incremental(
        producer,
        corpus,
        &pending,
        signer,
        &config.incremental_params,
    )?;

    // 2. Publish the freshly built incremental (if any), then mark it published — only AFTER the artifact
    //    is visible at the served root. The manifest (step 3) then advances over an existing artifact.
    let freshly_published = if let Some(report) = &incremental {
        if fault == PublishFault::AfterStageBeforePublish {
            // Reproduce a crash AFTER the catalog row + staged artifact, BEFORE publish.
            return Err(BuildError::Storage(StorageError::Generations {
                message: "injected publish fault (test seam): after stage, before publish"
                    .to_owned(),
            }));
        }
        publish_package(published_root, corpus, &report.package_id, &pending)?;
        mark_package_published(producer, corpus, &report.package_id)?;
        clean_dir(&pending)?; // staged artifact is now durably published
        Some(report.package_id.clone())
    } else {
        None
    };

    // 3. Rebuild + publish the signed remote manifest from the published artifacts.
    let signed = build_remote_manifest(
        producer,
        corpus,
        published_root,
        signer,
        &config.remote_manifest_params,
    )?;
    let remote_manifest_path = publish_remote_manifest(published_root, corpus, &signed)?;

    // 4. Report the published head coordinates (for the producer high-water-mark checkpoint).
    let (head_sequence, included_change_seq_high) = published_head(producer, corpus)?;

    Ok(ProducerCycleReport {
        corpus: corpus.to_owned(),
        built_incremental: freshly_published.or(resumed),
        head_sequence,
        included_change_seq_high,
        remote_manifest_path,
        enrichment: config.enrichment.clone(),
    })
}

/// Inputs for one [`rebaseline_cycle`] pass.
#[derive(Debug, Clone)]
pub struct RebaselineCycleConfig {
    pub baseline_params: BaselineParams,
    pub remote_manifest_params: RemoteManifestParams,
    pub enrichment: EnrichmentMode,
}

/// What one rebaseline cycle produced.
#[derive(Debug, Clone)]
pub struct RebaselineCycleReport {
    pub corpus: String,
    /// The published rebaseline package id (e.g. `core-1-2`).
    pub package_id: String,
    /// The `baseline_id` of the rebaseline package published this cycle. Under the M3 r3 design the
    /// rebaseline is ALWAYS freshly built from the current locked DB state (never resumed from a stale
    /// staged artifact), so this is ALWAYS the identity the current run intended — i.e. the published
    /// artifact's baseline set always equals the current run's pending per-source baselines, and the
    /// producer adopts each of them exactly (no identity-match heuristic).
    pub baseline_id: String,
    /// The superseding sequence window `from -> to` the rebaseline advances the corpus through.
    pub from_sequence: u64,
    pub to_sequence: u64,
    pub head_sequence: Option<u64>,
    pub included_change_seq_high: Option<u64>,
    pub remote_manifest_path: PathBuf,
    pub enrichment: EnrichmentMode,
}

/// Run one RE-BASELINE cycle (plan `02` Phase 5): build a full-snapshot `Rebaseline` artifact for
/// `corpus` from the producer's CURRENT authoritative tables (the new DILA baseline has already been
/// ingested), publish it, mark it published, then rebuild + publish the signed remote manifest. This is
/// the explicit, recorded adoption of a newer upstream baseline — it supersedes the chain forward
/// (`N -> N+1`, new generation) rather than crossing the boundary as an ordinary delta. It reuses the
/// SAME integrity / order / convergence primitives a normal cycle uses ([`build_rebaseline`] →
/// [`publish_package`] → [`build_remote_manifest`]).
///
/// # Errors
/// [`BuildError`] on a build/publish/manifest/DB/IO/signing failure, or if no baseline is cataloged yet
/// (a rebaseline supersedes an existing baseline; the first baseline is one-time operator setup).
pub fn rebaseline_cycle(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    signer: &dyn Signer,
    config: &RebaselineCycleConfig,
) -> Result<RebaselineCycleReport, BuildError> {
    rebaseline_cycle_faulted(
        producer,
        corpus,
        published_root,
        signer,
        config,
        PublishFault::None,
    )
}

/// [`rebaseline_cycle`] with a test fault seam. Hidden from docs; production callers use
/// [`rebaseline_cycle`].
///
/// # Errors
/// [`BuildError`] on a build/publish/manifest/DB/IO/signing failure (or the injected fault).
#[doc(hidden)]
pub fn rebaseline_cycle_faulted(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    signer: &dyn Signer,
    config: &RebaselineCycleConfig,
    fault: PublishFault,
) -> Result<RebaselineCycleReport, BuildError> {
    let pending = staged_pending_dir(published_root, corpus);

    // 0. Handle any staging slot a prior crashed cycle left behind, per the M3 r3 design:
    //    - a stranded INCREMENTAL is RESUMED (publish-once / chain integrity), so the rebaseline chains
    //      over a published head. `resume_pending` discards it instead if it is UNCATALOGED.
    //    - an incomplete prior REBASELINE attempt (cataloged-`"built"` OR uncataloged) is DISCARDED: its
    //      orphaned unpublished catalog row is deleted (so it can never be a chain head, conflict the
    //      fresh re-insert, or surface in the manifest) and its staging slot is cleared. We never resume a
    //      stale rebaseline — rebuilding from the current locked DB state is always safe.
    match read_staged_identity(&pending)? {
        Some(staged) if staged.package_kind == PackageKind::Rebaseline => {
            delete_unpublished_package_row(&mut producer.client()?, corpus, &staged.package_id)?;
            clean_dir(&pending)?;
        }
        Some(_incremental) => {
            resume_pending(producer, corpus, published_root, &pending)?;
        }
        None => {}
    }

    // 1. Build a FRESH rebaseline DIRECTLY into the durable staging slot from the current locked DB state
    //    and the last PUBLISHED head. Because it always reflects the CURRENT pending per-source baselines,
    //    the published artifact's baseline set == the current run's intended baseline set — so adoption is
    //    exact per-source with no stale-identity comparison.
    clean_dir(&pending)?;
    std::fs::create_dir_all(&pending)?;
    let built: RebaselineBuildReport =
        build_rebaseline(producer, corpus, &pending, signer, &config.baseline_params)?;

    if fault == PublishFault::AfterStageBeforePublish {
        // Reproduce a crash AFTER the catalog row + staged artifact, BEFORE publish — the next run must
        // DISCARD this incomplete attempt and rebuild fresh rather than chain a successor over it.
        return Err(BuildError::Storage(StorageError::Generations {
            message: "injected publish fault (test seam): after stage, before publish".to_owned(),
        }));
    }

    // 2. Publish the rebaseline artifact, then mark it published — only AFTER it is visible at the served
    //    root, so the manifest (next) advances over an existing artifact.
    publish_package(published_root, corpus, &built.package_id, &pending)?;
    mark_package_published(producer, corpus, &built.package_id)?;
    clean_dir(&pending)?;
    let (package_id, baseline_id, from_sequence, to_sequence) = (
        built.package_id,
        built.baseline_id,
        built.from_sequence,
        built.to_sequence,
    );

    // Rebuild + publish the signed remote manifest from the published artifacts (now headed by the
    // rebaseline). `active_baseline.package_kind` flips to `rebaseline` so a client routes the forward
    // re-anchor applier (plan P7), and existing + fresh sites converge through this one package.
    let signed = build_remote_manifest(
        producer,
        corpus,
        published_root,
        signer,
        &config.remote_manifest_params,
    )?;
    let remote_manifest_path = publish_remote_manifest(published_root, corpus, &signed)?;
    let (head_sequence, included_change_seq_high) = published_head(producer, corpus)?;

    Ok(RebaselineCycleReport {
        corpus: corpus.to_owned(),
        package_id,
        baseline_id,
        from_sequence,
        to_sequence,
        head_sequence,
        included_change_seq_high,
        remote_manifest_path,
        enrichment: config.enrichment.clone(),
    })
}

/// The identity of a staged-but-unpublished package read from the `pending` slot's embedded manifest. The
/// `package_kind` lets [`rebaseline_cycle`] route a stranded INCREMENTAL (resume, then build the rebaseline
/// on top) differently from an incomplete prior REBASELINE attempt (discard, then rebuild fresh).
struct StagedIdentity {
    package_id: String,
    package_kind: PackageKind,
}

/// Read the identity of a staged artifact in `pending`, or `None` when nothing is staged. Pure read — it
/// does not publish, mark, or clear the slot.
fn read_staged_identity(pending: &Path) -> Result<Option<StagedIdentity>, BuildError> {
    let manifest_path = jurisearch_package::artifact::manifest_path(pending);
    if !manifest_path.exists() {
        return Ok(None);
    }
    let bytes = std::fs::read(&manifest_path)?;
    let signed: Signed<EmbeddedManifest> = serde_json::from_slice(&bytes)?;
    let identity = &signed.payload.identity;
    Ok(Some(StagedIdentity {
        package_id: identity.package_id.clone(),
        package_kind: identity.package_kind,
    }))
}

/// Resume a staged-but-unpublished package left in `pending` by a prior crashed cycle: publish its
/// SAME `package_id` (idempotent — a no-op if it was already published) and mark the catalog row
/// published, then clear the slot. Returns the resumed package identity, or `None` when nothing was
/// staged OR the staged artifact was UNCATALOGED (an incomplete build, discarded — Codex r3 BLOCKER 2):
/// publishing+adopting a phantom the catalog-driven manifest could never reference must never happen.
fn resume_pending(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    pending: &Path,
) -> Result<Option<StagedIdentity>, BuildError> {
    let Some(staged) = read_staged_identity(pending)? else {
        return Ok(None);
    };
    // VERIFY a matching catalog row exists before publishing. A crash AFTER the staged manifest was
    // written but BEFORE the catalog insert leaves an UNCATALOGED artifact — discard it and build fresh.
    if package_catalog_status(&mut producer.client()?, corpus, &staged.package_id)?.is_none() {
        clean_dir(pending)?;
        return Ok(None);
    }
    // `publish_package` is idempotent: it converges a staged artifact and no-ops an already-published one.
    publish_package(published_root, corpus, &staged.package_id, pending)?;
    mark_package_published(producer, corpus, &staged.package_id)?;
    clean_dir(pending)?;
    Ok(Some(staged))
}

/// Mark a catalog row `"published"` (idempotent re-mark of an already-published row still matches its one
/// row). Asserts the UPDATE matched EXACTLY ONE row: a 0-row update means the package was never cataloged
/// (a phantom), which must fail loudly rather than silently advance the manifest (Codex r3 BLOCKER 2).
fn mark_package_published(
    producer: &impl DbClientSource,
    corpus: &str,
    package_id: &str,
) -> Result<(), BuildError> {
    let mut db = producer.client()?;
    let updated = db.execute(
        "UPDATE package_catalog SET status = 'published' WHERE corpus = $1 AND package_id = $2;",
        &[&corpus, &package_id],
    )?;
    if updated != 1 {
        return Err(BuildError::Storage(StorageError::Generations {
            message: format!(
                "mark_package_published matched {updated} rows for corpus `{corpus}` package \
                 `{package_id}` (expected exactly 1 — an uncataloged/phantom package must not publish)"
            ),
        }));
    }
    Ok(())
}

/// The newest cataloged package's `(package_sequence, included_change_seq_high)` for `corpus`, or
/// `(None, None)` when the corpus has no catalog rows yet.
fn published_head(
    producer: &impl DbClientSource,
    corpus: &str,
) -> Result<(Option<u64>, Option<u64>), BuildError> {
    let mut db = producer.client()?;
    let rows = catalog_rows_for_corpus(&mut db, corpus)?;
    Ok(rows.last().map_or((None, None), |row| {
        (
            Some(u64::try_from(row.package_sequence).unwrap_or(0)),
            Some(u64::try_from(row.included_change_seq_high).unwrap_or(0)),
        )
    }))
}

/// Remove a directory and everything under it (a no-op if it does not exist).
fn clean_dir(dir: &Path) -> Result<(), BuildError> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(BuildError::Io(error)),
    }
}
