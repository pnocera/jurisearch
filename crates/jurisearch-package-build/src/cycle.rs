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
use jurisearch_package::crypto::Verifier;
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::signed::Signed;

use jurisearch_storage::backend::DbClientSource;
use jurisearch_storage::generations::generation_name;
use jurisearch_storage::package_catalog::{
    CatalogRow, acquire_corpus_build_lock, catalog_rows_for_corpus, delete_unpublished_package_row,
    latest_media_package_for_corpus, package_catalog_status, release_corpus_build_lock,
};
use jurisearch_storage::runtime::StorageError;

use crate::baseline::{BaselineParams, RebaselineBuildReport, build_baseline, build_rebaseline};
use crate::error::BuildError;
use crate::incremental::{IncrementalParams, build_incremental};
use crate::publish::{
    publish_package, publish_remote_manifest, published_manifest_path, published_package_dir,
    staged_pending_dir,
};
use crate::remote_manifest::{
    RemoteManifestParams, build_remote_manifest, verify_catalog_identity,
};
use crate::verify::{verify_published_root, verify_signed_remote_manifest};

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

// --- First-baseline bootstrap (publish an EXISTING in-DB corpus as the FIRST signed baseline) ---

/// Inputs for [`bootstrap_first_baseline`]. Mirrors [`RebaselineCycleConfig`] minus enrichment (a
/// bootstrap publishes the corpus already in `public`; it runs no enrich/embed pass). The
/// `baseline_params.embedding_fingerprint`/`embedding_model`/`embedding_dimension` are the storage
/// embedding contract the preflight checks the actual DB rows against.
#[derive(Debug, Clone)]
pub struct BootstrapBaselineConfig {
    pub baseline_params: BaselineParams,
    pub remote_manifest_params: RemoteManifestParams,
}

/// What a first-baseline bootstrap published (identity + the self-verification result).
#[derive(Debug, Clone)]
pub struct BootstrapBaselineReport {
    pub corpus: String,
    pub package_id: String,
    pub generation: String,
    pub baseline_id: String,
    /// Always 1 — the canonical first-baseline package sequence.
    pub sequence: u64,
    pub included_change_seq_high: u64,
    pub total_rows: u64,
    /// The published artifact's aggregate payload digest (its embedded `integrity.artifact_sha256`).
    pub artifact_sha256: String,
    pub remote_manifest_path: PathBuf,
    pub head_sequence: u64,
    /// Artifacts the post-publish [`verify_published_root`] checked (the baseline + retained packages).
    pub artifacts_verified: usize,
}

/// A TEST-ONLY fault seam for [`bootstrap_first_baseline`], mirroring [`PublishFault`]: where (if
/// anywhere) to inject a simulated crash so a gated test can prove the resumable / no-partial-publish
/// bootstrap contract. Each variant reproduces a distinct crash window the finalize path must recover.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootstrapFault {
    /// Production: no injected fault.
    None,
    /// Fail right AFTER [`build_baseline`] catalogs the `built` row + stages the artifact, BEFORE
    /// [`publish_package`] — the artifact exists ONLY in the `.staging` slot.
    AfterCatalogInsert,
    /// Fail AFTER [`publish_package`] copies the artifact to `packages/`, BEFORE `mark_package_published`
    /// — the artifact is served but the catalog row is still `built`.
    AfterPublishPackage,
    /// Fail AFTER `mark_package_published` (row `published`, artifact served), BEFORE the manifest phase —
    /// a served, published artifact with NO client-visible `manifest.json`.
    AfterMarkPublished,
    /// Fail AFTER the in-memory pre-rename verify succeeds, BEFORE [`publish_remote_manifest`] — the
    /// manifest verified but never became client-visible.
    BeforeManifestRename,
}

/// Publish the EXISTING in-DB `corpus` (already ingested + embedded in `public`) as the producer's FIRST
/// signed baseline — WITHOUT fetching or re-embedding. Mirrors [`rebaseline_cycle`]'s structure (so the
/// in-module [`mark_package_published`] stays private) but for the bootstrap case where NO media root is
/// cataloged yet.
///
/// The locked span ([`bootstrap_locked`]) decides BUILD-vs-RESUME-vs-REFUSE from the newest `core` media
/// catalog row, so a crash between cataloging a `built`/`published` row and writing `manifest.json` is
/// RECOVERABLE on a re-run (the finalize is idempotent), never a permanent dead-end. In order: take the
/// per-corpus package build advisory lock; classify + (build OR resume); release the build lock; then
/// [`build_remote_manifest`] (which re-acquires that same advisory lock transiently on its OWN connection
/// — so it MUST run after we release, never while we hold it); VERIFY the in-memory signed manifest +
/// every referenced artifact BEFORE it becomes client-visible; atomically [`publish_remote_manifest`];
/// then self-verify the served root with the PUBLIC `verifier` as a final readback.
///
/// The safety property is that no client-visible `manifest.json` is published until the artifact it
/// references exists at the served root AND the pre-rename verify accepts it.
///
/// # Errors
/// [`BuildError`] on a failed preflight (schema/embedding/fingerprint), a repair-refusal (a divergent or
/// unrecoverable existing media row), an already-baselined refusal, or a build/publish/manifest/verify/
/// DB/IO/signing failure.
pub fn bootstrap_first_baseline(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    signer: &dyn Signer,
    verifier: &dyn Verifier,
    config: &BootstrapBaselineConfig,
) -> Result<BootstrapBaselineReport, BuildError> {
    bootstrap_first_baseline_faulted(
        producer,
        corpus,
        published_root,
        signer,
        verifier,
        config,
        BootstrapFault::None,
    )
}

/// [`bootstrap_first_baseline`] with a test fault seam. Hidden from docs; production callers use
/// [`bootstrap_first_baseline`].
///
/// # Errors
/// [`BuildError`] on any bootstrap failure (or the injected fault).
#[doc(hidden)]
pub fn bootstrap_first_baseline_faulted(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    signer: &dyn Signer,
    verifier: &dyn Verifier,
    config: &BootstrapBaselineConfig,
    fault: BootstrapFault,
) -> Result<BootstrapBaselineReport, BuildError> {
    let uri_base = &config.remote_manifest_params.uri_base;

    // A durable staging slot under the served root (the served layout — `manifest.json` + `packages/` —
    // never references this dot-dir), so a crash before publish leaves a recoverable, non-served artifact.
    let staging = published_root
        .join(corpus)
        .join(".staging")
        .join("bootstrap-baseline");

    // Serialize the classify + build/resume + catalog span against any concurrent package build for this
    // corpus. Released BEFORE the remote-manifest build, which re-acquires it on its own connection.
    let mut lock_db = producer.client()?;
    acquire_corpus_build_lock(&mut lock_db, corpus)?;
    let outcome = bootstrap_locked(
        producer,
        corpus,
        published_root,
        signer,
        verifier,
        &staging,
        config,
        fault,
    );
    let _ = release_corpus_build_lock(&mut lock_db, corpus);
    let identity = outcome?;

    // Build the signed remote manifest from the published artifacts. `build_remote_manifest` re-acquires
    // the build lock transiently and binds the catalog identity to the published artifact.
    let signed = build_remote_manifest(
        producer,
        corpus,
        published_root,
        signer,
        &config.remote_manifest_params,
    )?;

    // VERIFY the in-memory manifest + every referenced artifact BEFORE it becomes client-visible — the
    // referenced artifacts already exist under `packages/`, so resolving each `artifact_uri` to disk
    // works pre-rename. Only a manifest that verifies is ever atomically published.
    verify_signed_remote_manifest(published_root, corpus, uri_base, &signed, verifier)?;
    if fault == BootstrapFault::BeforeManifestRename {
        return Err(bootstrap_fail(
            "injected bootstrap fault (test seam): after pre-rename verify, before manifest rename"
                .to_owned(),
        ));
    }

    // Atomic temp-then-rename publish, so a reader never sees a manifest referencing a half-staged
    // artifact, then self-verify the served root exactly as a client would (the final readback).
    let remote_manifest_path = publish_remote_manifest(published_root, corpus, &signed)?;
    let verified = verify_published_root(published_root, corpus, uri_base, verifier)?;

    let artifact_sha256 = published_artifact_sha256(published_root, corpus, &identity.package_id)?;
    let (head_sequence, _included) = published_head(producer, corpus)?;

    Ok(BootstrapBaselineReport {
        corpus: corpus.to_owned(),
        package_id: identity.package_id,
        generation: identity.generation,
        baseline_id: identity.baseline_id,
        sequence: 1,
        included_change_seq_high: identity.included_change_seq_high,
        total_rows: identity.total_rows,
        artifact_sha256,
        remote_manifest_path,
        head_sequence: head_sequence.unwrap_or(1),
        artifacts_verified: verified.artifacts_checked,
    })
}

/// The identity of the first baseline the locked span built or resumed — the coordinates the finalize
/// phase needs after the build lock is released.
struct BootstrapIdentity {
    package_id: String,
    generation: String,
    baseline_id: String,
    included_change_seq_high: u64,
    total_rows: u64,
}

/// The locked span of [`bootstrap_first_baseline_faulted`]: classify the newest `core` media row, then
/// take ONE of three paths (all inside the per-corpus build lock):
/// 1. NO media row → FULL BUILD (embedding/schema preflight, then build → publish → mark → clean).
/// 2. a media row that is NOT the canonical first baseline → PRECISE REPAIR-REFUSAL.
/// 3. the canonical first baseline → REFUSE if its served manifest already verifies (already baselined),
///    otherwise RESUME the finalize without rebuilding (recover the published artifact, mark published).
#[allow(clippy::too_many_arguments)]
fn bootstrap_locked(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    signer: &dyn Signer,
    verifier: &dyn Verifier,
    staging: &Path,
    config: &BootstrapBaselineConfig,
    fault: BootstrapFault,
) -> Result<BootstrapIdentity, BuildError> {
    let params = &config.baseline_params;
    let uri_base = &config.remote_manifest_params.uri_base;

    let media = latest_media_package_for_corpus(&mut producer.client()?, corpus)?;
    let Some(row) = media else {
        // PATH 1 — FULL BUILD: no `core` media row exists yet (the canonical first-baseline path).
        bootstrap_preflight(producer, params)?;
        clean_dir(staging)?;
        std::fs::create_dir_all(staging)?;
        let report = build_baseline(producer, corpus, staging, signer, params)?;
        if fault == BootstrapFault::AfterCatalogInsert {
            // Crash AFTER the `built` catalog row + staged artifact, BEFORE publish (artifact only in
            // staging) — a re-run must RESUME by publishing the staged artifact.
            return Err(bootstrap_fail(
                "injected bootstrap fault (test seam): after catalog insert, before publish"
                    .to_owned(),
            ));
        }
        publish_package(published_root, corpus, &report.package_id, staging)?;
        if fault == BootstrapFault::AfterPublishPackage {
            // Crash AFTER publish, BEFORE mark (served artifact, row still `built`).
            return Err(bootstrap_fail(
                "injected bootstrap fault (test seam): after publish, before mark published"
                    .to_owned(),
            ));
        }
        mark_package_published(producer, corpus, &report.package_id)?;
        if fault == BootstrapFault::AfterMarkPublished {
            // Crash AFTER mark (served, published artifact), BEFORE the manifest phase (no manifest yet).
            return Err(bootstrap_fail(
                "injected bootstrap fault (test seam): after mark published, before manifest"
                    .to_owned(),
            ));
        }
        clean_dir(staging)?; // the artifact is now durably published under `packages/`
        return Ok(BootstrapIdentity {
            package_id: report.package_id,
            generation: report.generation,
            baseline_id: report.baseline_id,
            included_change_seq_high: report.included_change_seq_high,
            total_rows: report.total_rows,
        });
    };

    // A media row exists. First classify it: a NON-canonical row is a manual-repair situation (PATH 2).
    classify_first_baseline_row(&row, corpus, params)?;

    // PATH 3 — it IS the canonical first baseline. If the served manifest already verifies cleanly the
    // bootstrap is complete: REFUSE (this keeps the gated "second run is refused" tests valid).
    if published_manifest_path(published_root, corpus).exists()
        && verify_published_root(published_root, corpus, uri_base, verifier).is_ok()
    {
        return Err(bootstrap_fail(format!(
            "a `{corpus}` first baseline `{}` is already published and its served manifest verifies; \
             nothing to resume — refusing to republish",
            row.package_id
        )));
    }

    // RESUME: the manifest is missing OR failed verification. Finalize WITHOUT rebuilding.
    resume_first_baseline(producer, corpus, published_root, staging, &row)
}

/// PATH 2 guard: a media row that is NOT the canonical first baseline is a manual-repair situation — fail
/// with a PRECISE message naming the diverging field (distinct from the "already baselined" refusal).
fn classify_first_baseline_row(
    row: &CatalogRow,
    corpus: &str,
    params: &BaselineParams,
) -> Result<(), BuildError> {
    let expected_id = format!("{corpus}-1-1");
    let expected_generation = generation_name(corpus, 1);
    let diverging = if row.package_id != expected_id {
        Some(format!(
            "package_id `{}` != `{expected_id}`",
            row.package_id
        ))
    } else if row.package_sequence != 1 {
        Some(format!("package_sequence {} != 1", row.package_sequence))
    } else if row.package_kind != "baseline" {
        Some(format!("package_kind `{}` != `baseline`", row.package_kind))
    } else if row.generation != expected_generation {
        Some(format!(
            "generation `{}` != `{expected_generation}`",
            row.generation
        ))
    } else if row.baseline_id != params.baseline_id {
        Some(format!(
            "baseline_id `{}` != `{}`",
            row.baseline_id, params.baseline_id
        ))
    } else if row.embedding_fingerprint != params.embedding_fingerprint {
        Some(format!(
            "embedding_fingerprint `{}` != `{}`",
            row.embedding_fingerprint, params.embedding_fingerprint
        ))
    } else if row.status != "built" && row.status != "published" {
        Some(format!(
            "status `{}` is neither `built` nor `published`",
            row.status
        ))
    } else {
        None
    };
    if let Some(field) = diverging {
        return Err(bootstrap_fail(format!(
            "an existing `{corpus}` media row diverges from the canonical first baseline ({field}); \
             manual repair required before a bootstrap can resume — refusing to rebuild over it"
        )));
    }
    Ok(())
}

/// PATH 3 RESUME: finalize a canonical-but-unfinished first baseline WITHOUT rebuilding. Recover the
/// published artifact (from `packages/`, else by publishing the `.staging` slot), bind it to its catalog
/// row, mark the row published if still `built`, and derive the build identity from the row + artifact.
fn resume_first_baseline(
    producer: &impl DbClientSource,
    corpus: &str,
    published_root: &Path,
    staging: &Path,
    row: &CatalogRow,
) -> Result<BootstrapIdentity, BuildError> {
    let package_id = &row.package_id;
    let package_dir = published_package_dir(published_root, corpus, package_id);
    let package_manifest = jurisearch_package::artifact::manifest_path(&package_dir);
    let staged_manifest = jurisearch_package::artifact::manifest_path(staging);

    if !package_manifest.exists() {
        // The artifact never reached `packages/`. The ONLY safe source is the staging slot (a crash after
        // the catalog insert, before publish). Anything else is unrecoverable — manual repair.
        if !staged_manifest.exists() {
            return Err(bootstrap_fail(format!(
                "cannot resume `{corpus}` first baseline `{package_id}`: the published artifact is \
                 absent from `packages/` and no `.staging/bootstrap-baseline` slot holds it — manual \
                 repair required"
            )));
        }
        publish_package(published_root, corpus, package_id, staging)?;
    }

    // Recover + bind the now-published artifact to its catalog row (digest + canonical-manifest digest +
    // identity). A mismatch is a manual-repair situation, distinct from the "already baselined" refusal.
    let bytes = std::fs::read(&package_manifest)?;
    let signed: Signed<EmbeddedManifest> = serde_json::from_slice(&bytes)?;
    verify_catalog_identity(row, &signed, false).map_err(|error| {
        bootstrap_fail(format!(
            "cannot resume `{corpus}` first baseline `{package_id}`: the published artifact does not \
             match its catalog row ({error}); manual repair required"
        ))
    })?;

    if row.status == "built" {
        mark_package_published(producer, corpus, package_id)?;
    }
    clean_dir(staging)?;

    let total_rows = signed
        .payload
        .apply
        .postconditions
        .row_counts
        .values()
        .sum();
    Ok(BootstrapIdentity {
        package_id: row.package_id.clone(),
        generation: row.generation.clone(),
        baseline_id: row.baseline_id.clone(),
        included_change_seq_high: u64::try_from(row.included_change_seq_high).unwrap_or(0),
        total_rows,
    })
}

/// Fail-closed media coverage/schema preflight for a REBASELINE (`--from-db` snapshot-only) publish —
/// the same real-DB embedding/schema preconditions [`bootstrap_first_baseline`] runs before a first
/// baseline, exposed under a rebaseline-neutral name (Codex adjustment 4). It asserts, against the
/// producer's `public` tables under `params`' embedding contract, that: `schema_migrations` is current;
/// every `chunks` row has a matching `chunk_embeddings` row under the configured fingerprint/model/
/// dimension; every `chunks.embedding_fingerprint` equals the configured fingerprint; and the same
/// coverage + consistency for `zone_units`. It NEVER mutates. A snapshot-only rebaseline deliberately
/// SKIPS the embed/finalize passes, so this guard is what keeps it from publishing an under-embedded or
/// fingerprint-inconsistent corpus (fail closed).
///
/// # Errors
/// [`BuildError`] (mapped to the producer `publish-failed` class) on a stale schema, missing/inconsistent
/// chunk embeddings, or missing/inconsistent zone-unit embeddings.
pub fn rebaseline_preflight(
    producer: &impl DbClientSource,
    params: &BaselineParams,
) -> Result<(), BuildError> {
    bootstrap_preflight(producer, params)
}

/// Fail-closed real-DB embedding/schema preconditions for a FULL first-baseline build (Codex adjustment
/// 7). Each rejection carries an exact, actionable message; none mutate. Verified against the producer's
/// `public` tables under the config-supplied embedding contract. The existing-media-row decision (build
/// vs resume vs refuse) is made by [`bootstrap_locked`] BEFORE this runs, so a resume never re-checks
/// embeddings — this only guards the path that actually builds a fresh baseline. The public
/// [`rebaseline_preflight`] wrapper reuses this exact check for the snapshot-only rebaseline path.
fn bootstrap_preflight(
    producer: &impl DbClientSource,
    params: &BaselineParams,
) -> Result<(), BuildError> {
    let mut db = producer.client()?;

    // 1. The producer schema must be exactly what this binary builds for. `schema_migrations.version`
    //    is `integer` (int4), so it MUST be read as `i32` — reading it as `i64` panics in rust-postgres
    //    with a deserialization error (the migration applier reads it the same way).
    let schema_version: i32 = db
        .query_one(
            "SELECT coalesce(max(version), 0) FROM schema_migrations;",
            &[],
        )?
        .get(0);
    let expected = jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION;
    if schema_version != expected {
        return Err(bootstrap_fail(format!(
            "producer schema_version {schema_version} != CURRENT_SCHEMA_VERSION {expected}; migrate \
             the producer database before publishing the first baseline"
        )));
    }

    // 2. EVERY chunk must have a matching embedding under the publish fingerprint/model/dimension, and
    //    `chunks.embedding_fingerprint` must be stamped consistently — else the baseline is package-valid
    //    but query-incomplete under a client's bge-m3 query embedder.
    let dimension = i32::try_from(params.embedding_dimension).unwrap_or(i32::MAX);
    let missing_chunks: i64 = db
        .query_one(
            "SELECT count(*) FROM chunks c \
             LEFT JOIN chunk_embeddings e ON e.chunk_id = c.chunk_id \
               AND e.embedding_fingerprint = $1 AND e.model = $2 AND e.dimension = $3 \
             WHERE e.chunk_id IS NULL;",
            &[
                &params.embedding_fingerprint,
                &params.embedding_model,
                &dimension,
            ],
        )?
        .get(0);
    if missing_chunks > 0 {
        return Err(bootstrap_fail(format!(
            "{missing_chunks} `chunks` row(s) have no matching `chunk_embeddings` under fingerprint \
             `{}` / model `{}` / dimension {dimension}; the corpus is not fully embedded — re-embed \
             before publishing",
            params.embedding_fingerprint, params.embedding_model
        )));
    }
    let inconsistent_chunks: i64 = db
        .query_one(
            "SELECT count(*) FROM chunks WHERE embedding_fingerprint IS DISTINCT FROM $1;",
            &[&params.embedding_fingerprint],
        )?
        .get(0);
    if inconsistent_chunks > 0 {
        return Err(bootstrap_fail(format!(
            "{inconsistent_chunks} `chunks` row(s) carry an embedding_fingerprint other than `{}`; the \
             stored fingerprint is inconsistent with the publish fingerprint",
            params.embedding_fingerprint
        )));
    }

    // 3. Same coverage + consistency for zone units (an empty zone set trivially passes).
    let missing_zones: i64 = db
        .query_one(
            "SELECT count(*) FROM zone_units z \
             LEFT JOIN zone_unit_embeddings e ON e.zone_unit_id = z.zone_unit_id \
               AND e.embedding_fingerprint = $1 AND e.model = $2 AND e.dimension = $3 \
             WHERE e.zone_unit_id IS NULL;",
            &[
                &params.embedding_fingerprint,
                &params.embedding_model,
                &dimension,
            ],
        )?
        .get(0);
    if missing_zones > 0 {
        return Err(bootstrap_fail(format!(
            "{missing_zones} `zone_units` row(s) have no matching `zone_unit_embeddings` under \
             fingerprint `{}` / model `{}` / dimension {dimension}; zone units are not fully embedded",
            params.embedding_fingerprint, params.embedding_model
        )));
    }
    let inconsistent_zones: i64 = db
        .query_one(
            "SELECT count(*) FROM zone_units WHERE embedding_fingerprint IS DISTINCT FROM $1;",
            &[&params.embedding_fingerprint],
        )?
        .get(0);
    if inconsistent_zones > 0 {
        return Err(bootstrap_fail(format!(
            "{inconsistent_zones} `zone_units` row(s) carry an embedding_fingerprint other than `{}`",
            params.embedding_fingerprint
        )));
    }
    Ok(())
}

/// The published artifact's aggregate payload digest (its embedded `integrity.artifact_sha256`).
fn published_artifact_sha256(
    root: &Path,
    corpus: &str,
    package_id: &str,
) -> Result<String, BuildError> {
    let dir = published_package_dir(root, corpus, package_id);
    let bytes = std::fs::read(jurisearch_package::artifact::manifest_path(&dir))?;
    let signed: Signed<EmbeddedManifest> = serde_json::from_slice(&bytes)?;
    Ok(signed.payload.integrity.artifact_sha256)
}

/// A preflight/bootstrap rejection (mapped to the producer `publish-failed` class by the caller).
fn bootstrap_fail(message: String) -> BuildError {
    BuildError::Storage(StorageError::Generations { message })
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
