//! The size-driven catch-up planner + orchestration loop (plan P7, design §9.4 / §11.1).
//!
//! A client polls a SIGNED remote manifest, then [`plan_catchup`] (a pure function over the verified
//! manifest + the local cursor) decides **incremental chain vs fresh baseline vs blocked** by
//! cumulative byte size + estimated apply cost — NOT chain length (§9.4). [`run_catchup`] then fetches
//! each artifact through a [`CatchupSource`] seam (local dir for tests; network/TLS is P9), verifies it
//! against the signed remote digest, and applies in order — relying on the P4/P5/P6 apply gates
//! (signature, entitlement, per-file + postcondition digests, in-order `SequenceGap`) as the
//! authoritative trust boundary. Planner pre-filtering only avoids doomed downloads; it never replaces
//! the apply gates (§11.1: warn-and-reject, no partial cursor movement).

use std::path::{Path, PathBuf};

use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::manifest::remote::{
    BaselineRef, CatchupMode, RemoteManifest, RemotePackageEntry,
};
use jurisearch_package::sequence::PackageSequence;
use jurisearch_package::signed::Signed;
use jurisearch_package::{PackageKind, RejectCode, Verifier};
use jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION;
use jurisearch_storage::runtime::ManagedPostgres;

use crate::apply::CLIENT_VERSION;
use crate::error::SyncError;
use crate::{BaselineApplyOutcome, apply_baseline, apply_incremental, apply_rebaseline};

/// The full client cursor stamps from `jurisearch_control.corpus_state` — richer than `CorpusStatus`
/// (it carries the `embedding_fingerprint` / `builder_versions` the planner needs for reissue
/// detection, plus the chain-link identity).
#[derive(Debug, Clone)]
pub struct ClientCursor {
    pub corpus: String,
    pub sequence: u64,
    pub active_generation: String,
    pub baseline_id: String,
    pub schema_version: i32,
    pub embedding_fingerprint: String,
    pub builder_versions: serde_json::Value,
    pub last_package_id: Option<String>,
    pub last_package_digest: Option<String>,
}

/// Read the full cursor for `corpus`, or `None` if the corpus is not installed.
///
/// # Errors
/// [`SyncError`] on a DB error.
pub fn read_client_cursor(
    client: &ManagedPostgres,
    corpus: &str,
) -> Result<Option<ClientCursor>, SyncError> {
    let mut db = client.client()?;
    let row = db
        .query_opt(
            "SELECT sequence, active_generation, baseline_id, schema_version, embedding_fingerprint, \
                    builder_versions, last_package_id, last_package_digest \
             FROM jurisearch_control.corpus_state WHERE corpus = $1;",
            &[&corpus],
        )
        .map_err(SyncError::Postgres)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let sequence: i64 = row.get("sequence");
    Ok(Some(ClientCursor {
        corpus: corpus.to_owned(),
        sequence: u64::try_from(sequence).unwrap_or(0),
        active_generation: row.get("active_generation"),
        baseline_id: row.get("baseline_id"),
        schema_version: row.get("schema_version"),
        embedding_fingerprint: row.get("embedding_fingerprint"),
        builder_versions: row.get("builder_versions"),
        last_package_id: row.get("last_package_id"),
        last_package_digest: row.get("last_package_digest"),
    }))
}

/// The catch-up decision (§9.4). Every variant is derived purely from the verified manifest + cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatchupPlan {
    /// The cursor is already at the manifest head.
    UpToDate,
    /// Load the active baseline (fresh client, past retention, or a §9.4 baseline-preferred condition).
    FreshBaseline(BaselineRef),
    /// Apply this gap-free, in-order incremental chain `(cursor, head]`.
    Incremental(Vec<RemotePackageEntry>),
    /// The client cannot proceed (must upgrade / wrong feed / malformed manifest); carries the §6.3 code.
    Blocked { code: RejectCode, reason: String },
}

/// Decide catch-up for a corpus from a SIGNED-then-verified remote manifest + the local cursor
/// (`None` = corpus not installed). Pure: the only client facts used are [`CLIENT_VERSION`] and
/// [`CURRENT_SCHEMA_VERSION`]; thresholds come from the manifest's `catchup_policy` (tunable without a
/// client upgrade). The caller MUST have verified `Signed<RemoteManifest>` first.
#[must_use]
pub fn plan_catchup(manifest: &RemoteManifest, cursor: Option<&ClientCursor>) -> CatchupPlan {
    let head = manifest.head_sequence.get();

    // Manifest sanity: the active baseline cannot be ahead of the head.
    if manifest.active_baseline.sequence.get() > head {
        return blocked(
            RejectCode::WrongGeneration,
            "malformed remote manifest: active_baseline is ahead of head_sequence",
        );
    }

    // Fresh client → the active baseline (if the client can apply it).
    let Some(cursor) = cursor else {
        return baseline_compat_or_blocked(&manifest.active_baseline);
    };

    if cursor.sequence == head {
        return CatchupPlan::UpToDate;
    }
    if cursor.sequence > head {
        return blocked(
            RejectCode::WrongGeneration,
            format!(
                "client sequence {} is ahead of remote head {head} (wrong feed/environment)",
                cursor.sequence
            ),
        );
    }

    // Past the retention window → a forward re-baseline supersession (or Blocked if none is available).
    if manifest.requires_baseline_for(PackageSequence::new(cursor.sequence)) {
        return installed_baseline_or_blocked(&manifest.active_baseline, cursor);
    }

    // Reconstruct the gap-free chain (cursor, head]; a gap/dup/non-monotonic feed → baseline.
    let Some(chain) = build_chain(manifest, cursor.sequence, head) else {
        return installed_baseline_or_blocked(&manifest.active_baseline, cursor);
    };

    // An explicit SIGNED `RequiresBaseline` routing range covering the client position is authoritative.
    if requires_baseline_range(manifest, cursor.sequence) {
        return installed_baseline_or_blocked(&manifest.active_baseline, cursor);
    }

    // Version gate: a chain entry needing a newer client cannot be applied (and a baseline won't fix it).
    for entry in &chain {
        if !CLIENT_VERSION.satisfies_minimum(entry.minimum_client_version) {
            return blocked(
                RejectCode::ClientTooOld,
                format!(
                    "package {} requires a newer client than {CLIENT_VERSION:?}",
                    entry.package_id
                ),
            );
        }
    }

    // Schema-ahead: a chain entry the client's binary cannot understand. A too-new baseline can't fix a
    // too-new schema, so route to baseline ONLY if a forward re-baseline supersession is actually
    // applicable here; otherwise the client must upgrade (`SchemaAhead`).
    for entry in &chain {
        if entry.schema_version > CURRENT_SCHEMA_VERSION {
            let fallback = installed_baseline_or_blocked(&manifest.active_baseline, cursor);
            if matches!(fallback, CatchupPlan::FreshBaseline(_)) {
                return fallback;
            }
            return blocked(
                RejectCode::SchemaAhead,
                format!(
                    "package {} schema {} is ahead of this client ({CURRENT_SCHEMA_VERSION}); upgrade required",
                    entry.package_id, entry.schema_version
                ),
            );
        }
    }

    // §9.4 "prefer baseline when ANY" — but only a forward re-baseline supersession can actually catch
    // an installed client up; if none is available the client is Blocked (the producer must re-baseline).
    if prefer_baseline(manifest, cursor, &chain) {
        return installed_baseline_or_blocked(&manifest.active_baseline, cursor);
    }

    CatchupPlan::Incremental(chain)
}

/// Route a FRESH client (no cursor) to the active baseline, unless the client binary cannot apply it.
/// A fresh client can apply EITHER a first `baseline` or a `rebaseline` media root (the applier's
/// `FirstBaseline` / `RebaselineForward` guards both accept an absent cursor).
fn baseline_compat_or_blocked(baseline: &BaselineRef) -> CatchupPlan {
    if !CLIENT_VERSION.satisfies_minimum(baseline.minimum_client_version) {
        return blocked(
            RejectCode::ClientTooOld,
            format!(
                "baseline {} requires a newer client than {CLIENT_VERSION:?}",
                baseline.baseline_id
            ),
        );
    }
    if baseline.schema_version > CURRENT_SCHEMA_VERSION {
        return blocked(
            RejectCode::SchemaAhead,
            format!(
                "baseline {} schema {} is ahead of this client ({CURRENT_SCHEMA_VERSION})",
                baseline.baseline_id, baseline.schema_version
            ),
        );
    }
    CatchupPlan::FreshBaseline(baseline.clone())
}

/// Route an INSTALLED client's baseline fallback (plan P7 r1 BLOCKER). An installed corpus can ONLY be
/// caught up by a FORWARD re-baseline supersession: a first-`baseline` package is rejected by the
/// applier's first-baseline guard, and a baseline at/behind the cursor cannot advance it (the
/// `RebaselineForward` guard rejects `current >= result`). When the active baseline cannot supersede,
/// the client is `Blocked { BaselineRequired }` until the producer publishes a re-baseline.
fn installed_baseline_or_blocked(baseline: &BaselineRef, cursor: &ClientCursor) -> CatchupPlan {
    if baseline.package_kind != PackageKind::Rebaseline {
        return blocked(
            RejectCode::BaselineRequired,
            format!(
                "installed corpus `{}` needs a re-baseline supersession, but the active media root \
                 `{}` is a first baseline (not catch-up-capable)",
                cursor.corpus, baseline.baseline_id
            ),
        );
    }
    if baseline.sequence.get() <= cursor.sequence {
        return blocked(
            RejectCode::BaselineRequired,
            format!(
                "active re-baseline {} (sequence {}) does not advance client `{}` at sequence {}",
                baseline.baseline_id,
                baseline.sequence.get(),
                cursor.corpus,
                cursor.sequence
            ),
        );
    }
    baseline_compat_or_blocked(baseline)
}

/// Build the ordered chain of entries linking `from` → `to` (`to` is the head). Returns `None` on a
/// gap, a duplicate `from_sequence`, or a non-`+1` link (an unreconstructable feed → baseline).
fn build_chain(manifest: &RemoteManifest, from: u64, to: u64) -> Option<Vec<RemotePackageEntry>> {
    let mut chain = Vec::new();
    let mut current = from;
    while current < to {
        let mut matches = manifest
            .packages
            .iter()
            .filter(|p| p.from_sequence.get() == current);
        let entry = matches.next()?;
        if matches.next().is_some() {
            return None; // duplicate edge from `current`
        }
        if entry.to_sequence.get() != current + 1 {
            return None; // non-+1 link (a media reissue is not an incremental chain edge)
        }
        chain.push(entry.clone());
        current = entry.to_sequence.get();
    }
    if current == to { Some(chain) } else { None }
}

/// Whether a SIGNED `RequiresBaseline` routing range covers `seq`. Only a BOUNDED range
/// (`from <= seq <= to`) forces baseline; an open-ended (`to: None`) `RequiresBaseline` range is
/// treated as redundant with `min_available_sequence` (already handled) and ignored, so a client that
/// can legitimately catch up incrementally is never forced to a full reload (plan P7).
fn requires_baseline_range(manifest: &RemoteManifest, seq: u64) -> bool {
    manifest.catchup_ranges.iter().any(|r| {
        r.mode == CatchupMode::RequiresBaseline
            && r.from_sequence.get() <= seq
            && r.to_sequence.is_some_and(|to| seq <= to.get())
    })
}

/// The §9.4 "prefer baseline when ANY" predicate over a reconstructed chain.
fn prefer_baseline(
    manifest: &RemoteManifest,
    cursor: &ClientCursor,
    chain: &[RemotePackageEntry],
) -> bool {
    let policy = &manifest.catchup_policy;

    // A package that itself requires a baseline (a media reissue listed in the feed).
    if chain.iter().any(|e| e.requires_baseline) {
        return true;
    }
    // The chain crosses a fingerprint / builder reissue relative to the cursor.
    let cursor_builders = &cursor.builder_versions;
    if chain.iter().any(|e| {
        e.embedding_fingerprint != cursor.embedding_fingerprint
            || serde_json::to_value(&e.builder_versions).ok().as_ref() != Some(cursor_builders)
    }) {
        return true;
    }
    // Chain length cap.
    if chain.len() as u64 > u64::from(policy.max_incremental_packages) {
        return true;
    }
    // Cumulative-size ratios (integer arithmetic, u128 products; zero-baseline → prefer baseline).
    let cum_compressed: u128 = chain
        .iter()
        .map(|e| u128::from(e.compressed_size_bytes))
        .sum();
    if ratio_exceeds(
        cum_compressed,
        u128::from(manifest.active_baseline.compressed_size_bytes),
        policy.max_cumulative_diff_to_baseline_permille,
    ) {
        return true;
    }
    let cum_uncompressed: u128 = chain
        .iter()
        .map(|e| u128::from(e.uncompressed_size_bytes))
        .sum();
    if ratio_exceeds(
        cum_uncompressed,
        u128::from(manifest.active_baseline.uncompressed_size_bytes),
        policy.max_cumulative_uncompressed_to_baseline_permille,
    ) {
        return true;
    }
    // Estimated apply cost: over the corpus budget, or exceeding the media baseline's load time.
    let cum_apply: u64 = chain
        .iter()
        .map(|e| u64::from(e.estimated_apply_seconds))
        .sum();
    if cum_apply > u64::from(policy.max_apply_seconds_budget) {
        return true;
    }
    if cum_apply > u64::from(manifest.active_baseline.estimated_load_seconds) {
        return true;
    }
    false
}

/// `cumulative * 1000 > baseline * permille` with u128 products. A zero baseline size with a positive
/// cumulative diff is treated as "prefer baseline" (a malformed/degenerate manifest, never div-by-zero).
fn ratio_exceeds(cumulative: u128, baseline: u128, permille: u32) -> bool {
    if baseline == 0 {
        return cumulative > 0;
    }
    cumulative.saturating_mul(1000) > baseline.saturating_mul(u128::from(permille))
}

fn blocked(code: RejectCode, reason: impl Into<String>) -> CatchupPlan {
    CatchupPlan::Blocked {
        code,
        reason: reason.into(),
    }
}

/// Where a planned artifact's bytes come from (plan P7). A local-directory implementation backs the
/// tests; the authenticated/TLS network implementation is P9. The fetched directory is verified against
/// the SIGNED remote digest by [`run_catchup`] before any apply.
pub trait CatchupSource {
    /// Fetch the active baseline artifact, returning its local directory.
    ///
    /// # Errors
    /// [`SyncError`] on a fetch failure.
    fn fetch_baseline(&self, baseline: &BaselineRef) -> Result<PathBuf, SyncError>;

    /// Fetch one incremental package artifact, returning its local directory.
    ///
    /// # Errors
    /// [`SyncError`] on a fetch failure.
    fn fetch_package(&self, entry: &RemotePackageEntry) -> Result<PathBuf, SyncError>;
}

/// The result of executing a [`CatchupPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatchupReport {
    UpToDate,
    BaselineApplied(BaselineApplyOutcome),
    IncrementalApplied { applied: usize },
}

/// Execute a [`CatchupPlan`]: fetch each artifact, verify it against the SIGNED remote digest, and
/// apply in order through the existing P4/P5/P6 appliers (which re-verify signature, entitlement,
/// per-file + postcondition digests, and enforce in-order `SequenceGap`). Stops on the first apply
/// failure with NO skipping of later packages (INV-2 gap-free).
///
/// # Errors
/// [`SyncError`] (with the §6.3 code) on a `Blocked` plan, a digest mismatch, or an apply refusal.
pub fn run_catchup(
    client: &ManagedPostgres,
    source: &dyn CatchupSource,
    verifier: &dyn Verifier,
    plan: CatchupPlan,
) -> Result<CatchupReport, SyncError> {
    match plan {
        CatchupPlan::UpToDate => Ok(CatchupReport::UpToDate),
        CatchupPlan::Blocked { code, reason } => Err(SyncError::reject(code, reason)),
        CatchupPlan::FreshBaseline(baseline) => {
            let dir = source.fetch_baseline(&baseline)?;
            verify_artifact_digest(&dir, &baseline.sha256)?;
            let outcome = apply_media_auto(client, &dir, verifier)?;
            Ok(CatchupReport::BaselineApplied(outcome))
        }
        CatchupPlan::Incremental(chain) => {
            let mut applied = 0;
            for entry in &chain {
                let dir = source.fetch_package(entry)?;
                verify_artifact_digest(&dir, &entry.sha256)?;
                // `apply_incremental` is the authoritative ordering guard (`SequenceGap`, chain-link,
                // preconditions, in-txn postconditions); a failure stops the loop, never skips ahead.
                apply_incremental(client, &dir, verifier)?;
                applied += 1;
            }
            Ok(CatchupReport::IncrementalApplied { applied })
        }
    }
}

/// Apply a media (baseline OR re-baseline) artifact, dispatching on the embedded manifest's kind so a
/// `FreshBaseline` plan calls the right applier (an installed long-offline client applying the active
/// re-baseline must NOT go through `apply_baseline`'s first-baseline guard). The embedded manifest is
/// re-verified by the chosen applier.
///
/// # Errors
/// [`SyncError`] on a non-media package or an apply refusal.
pub fn apply_media_auto(
    client: &ManagedPostgres,
    artifact_dir: &Path,
    verifier: &dyn Verifier,
) -> Result<BaselineApplyOutcome, SyncError> {
    let manifest = read_embedded_manifest(artifact_dir)?;
    match manifest.identity.package_kind {
        PackageKind::Baseline => apply_baseline(client, artifact_dir, verifier),
        PackageKind::Rebaseline => apply_rebaseline(client, artifact_dir, verifier),
        PackageKind::Incremental => Err(SyncError::reject(
            RejectCode::BaselineRequired,
            "apply_media_auto only dispatches baseline/re-baseline media packages",
        )),
    }
}

/// Bind a fetched artifact to its SIGNED remote digest (plan P7): the embedded manifest's
/// `integrity.artifact_sha256` (the aggregate over verified per-file digests — the logical artifact
/// digest for an unpacked artifact; real whole-archive hashing is P9 transport) must equal the remote
/// entry/ref `sha256`. Defends against a fetch source swapping a different (still-signed) artifact.
fn verify_artifact_digest(artifact_dir: &Path, expected_sha256: &str) -> Result<(), SyncError> {
    let manifest = read_embedded_manifest(artifact_dir)?;
    if manifest.integrity.artifact_sha256 != expected_sha256 {
        return Err(SyncError::reject(
            RejectCode::DigestMismatch,
            format!(
                "fetched artifact digest {} != remote-manifest sha256 {expected_sha256}",
                manifest.integrity.artifact_sha256
            ),
        ));
    }
    Ok(())
}

/// Read (WITHOUT verifying) the embedded manifest payload — used only to read the package kind / digest
/// for dispatch + binding; the applier re-verifies the signature before trusting any field.
fn read_embedded_manifest(artifact_dir: &Path) -> Result<EmbeddedManifest, SyncError> {
    let bytes = std::fs::read(jurisearch_package::artifact::manifest_path(artifact_dir))?;
    let signed: Signed<EmbeddedManifest> = serde_json::from_slice(&bytes)?;
    Ok(signed.payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jurisearch_package::compat::Version;
    use jurisearch_package::corpus::Corpus;
    use jurisearch_package::crypto::{KeyEpoch, KeyId, Signature};
    use jurisearch_package::manifest::remote::{
        CatchupPolicy, CatchupRange, EntitlementListing, EntitlementTier, SigningInfo,
    };
    use std::collections::BTreeMap;

    fn sig() -> Signature {
        Signature {
            algorithm: "ed25519".to_owned(),
            key_id: KeyId("k".to_owned()),
            key_epoch: KeyEpoch(1),
            signature_hex: String::new(),
        }
    }

    fn entry(from: u64, to: u64) -> RemotePackageEntry {
        RemotePackageEntry {
            package_id: format!("core-{from}-{to}"),
            from_sequence: PackageSequence::new(from),
            to_sequence: PackageSequence::new(to),
            artifact_uri: format!("https://h/core-{from}-{to}"),
            compressed_size_bytes: 10,
            uncompressed_size_bytes: 100,
            estimated_apply_seconds: 5,
            row_counts: BTreeMap::new(),
            requires_baseline: false,
            minimum_client_version: Version::new(0, 1, 0),
            schema_version: CURRENT_SCHEMA_VERSION,
            embedding_fingerprint: "fp".to_owned(),
            builder_versions: BTreeMap::new(),
            sha256: format!("sha256:{from}-{to}"),
            signature: sig(),
        }
    }

    fn baseline(seq: u64) -> BaselineRef {
        BaselineRef {
            baseline_id: "core-base".to_owned(),
            generation: "core_g0005".to_owned(),
            package_kind: PackageKind::Baseline,
            sequence: PackageSequence::new(seq),
            schema_version: CURRENT_SCHEMA_VERSION,
            minimum_client_version: Version::new(0, 1, 0),
            artifact_uri: "media://base".to_owned(),
            compressed_size_bytes: 1000,
            uncompressed_size_bytes: 10_000,
            estimated_load_seconds: 600,
            sha256: "sha256:base".to_owned(),
            signature: sig(),
        }
    }

    /// A FORWARD re-baseline media root — the only baseline that can catch up an INSTALLED client.
    fn rebaseline(seq: u64) -> BaselineRef {
        BaselineRef {
            package_kind: PackageKind::Rebaseline,
            ..baseline(seq)
        }
    }

    fn policy() -> CatchupPolicy {
        CatchupPolicy {
            max_incremental_packages: 100,
            max_cumulative_diff_to_baseline_permille: 330,
            max_cumulative_uncompressed_to_baseline_permille: 500,
            max_apply_seconds_budget: 2700,
        }
    }

    fn manifest(
        head: u64,
        min_avail: u64,
        base: BaselineRef,
        packages: Vec<RemotePackageEntry>,
    ) -> RemoteManifest {
        RemoteManifest {
            manifest_version: 1,
            generated_at: "2026-06-27T00:00:00Z".to_owned(),
            publisher: "jurisearch".to_owned(),
            corpus: Corpus::new("core").unwrap(),
            environment: "test".to_owned(),
            head_sequence: PackageSequence::new(head),
            min_available_sequence: PackageSequence::new(min_avail),
            active_baseline: base,
            packages,
            catchup_ranges: vec![],
            catchup_policy: policy(),
            entitlement: EntitlementListing {
                corpus: Corpus::new("core").unwrap(),
                tier: EntitlementTier::Open,
                license_epoch: 0,
                audience: None,
            },
            signing: SigningInfo {
                key_id: KeyId("k".to_owned()),
                algorithm: "ed25519".to_owned(),
            },
        }
    }

    fn cursor(seq: u64) -> ClientCursor {
        ClientCursor {
            corpus: "core".to_owned(),
            sequence: seq,
            active_generation: "core_g0005".to_owned(),
            baseline_id: "core-base".to_owned(),
            schema_version: CURRENT_SCHEMA_VERSION,
            embedding_fingerprint: "fp".to_owned(),
            builder_versions: serde_json::json!({}),
            last_package_id: None,
            last_package_digest: None,
        }
    }

    /// A 3-link chain 2→3→4→5 with a baseline at sequence 1.
    fn chain_manifest() -> RemoteManifest {
        manifest(
            5,
            2,
            baseline(1),
            vec![entry(2, 3), entry(3, 4), entry(4, 5)],
        )
    }

    #[test]
    fn fresh_client_gets_the_active_baseline() {
        assert!(matches!(
            plan_catchup(&chain_manifest(), None),
            CatchupPlan::FreshBaseline(b) if b.sequence.get() == 1
        ));
    }

    #[test]
    fn cursor_at_head_is_up_to_date() {
        assert_eq!(
            plan_catchup(&chain_manifest(), Some(&cursor(5))),
            CatchupPlan::UpToDate
        );
    }

    #[test]
    fn ahead_of_head_is_blocked_wrong_generation() {
        assert!(matches!(
            plan_catchup(&chain_manifest(), Some(&cursor(9))),
            CatchupPlan::Blocked {
                code: RejectCode::WrongGeneration,
                ..
            }
        ));
    }

    #[test]
    fn an_in_window_client_gets_the_ordered_incremental_chain() {
        let plan = plan_catchup(&chain_manifest(), Some(&cursor(2)));
        match plan {
            CatchupPlan::Incremental(chain) => {
                let ids: Vec<_> = chain.iter().map(|e| e.package_id.clone()).collect();
                assert_eq!(ids, vec!["core-2-3", "core-3-4", "core-4-5"]);
            }
            other => panic!("expected Incremental, got {other:?}"),
        }
    }

    #[test]
    fn a_client_past_the_retention_window_gets_a_forward_rebaseline() {
        // min_available 4 but the client is at 2; a FORWARD re-baseline at 6 can supersede it.
        let m = manifest(
            7,
            4,
            rebaseline(6),
            vec![entry(4, 5), entry(5, 6), entry(6, 7)],
        );
        assert!(matches!(
            plan_catchup(&m, Some(&cursor(2))),
            CatchupPlan::FreshBaseline(b) if b.sequence.get() == 6
        ));
    }

    #[test]
    fn a_gap_in_the_chain_routes_to_a_forward_rebaseline() {
        let m = manifest(5, 2, rebaseline(5), vec![entry(2, 3), entry(4, 5)]); // missing 3→4
        assert!(matches!(
            plan_catchup(&m, Some(&cursor(2))),
            CatchupPlan::FreshBaseline(_)
        ));
    }

    #[test]
    fn a_fingerprint_reissue_in_the_chain_routes_to_a_forward_rebaseline() {
        let mut reissue = entry(3, 4);
        reissue.embedding_fingerprint = "fp2".to_owned(); // crosses a re-embed boundary
        let m = manifest(5, 2, rebaseline(5), vec![entry(2, 3), reissue, entry(4, 5)]);
        assert!(matches!(
            plan_catchup(&m, Some(&cursor(2))),
            CatchupPlan::FreshBaseline(_)
        ));
    }

    #[test]
    fn a_requires_baseline_entry_routes_to_a_forward_rebaseline() {
        let mut reissue = entry(3, 4);
        reissue.requires_baseline = true;
        let m = manifest(5, 2, rebaseline(5), vec![entry(2, 3), reissue, entry(4, 5)]);
        assert!(matches!(
            plan_catchup(&m, Some(&cursor(2))),
            CatchupPlan::FreshBaseline(_)
        ));
    }

    #[test]
    fn an_installed_client_with_only_a_first_baseline_is_blocked() {
        // The active media root is a first `baseline` (not a re-baseline) — it cannot catch up an
        // installed corpus, so a baseline-preferred route is Blocked until the producer re-baselines.
        let m = manifest(5, 2, baseline(1), vec![entry(2, 3), entry(4, 5)]); // gap forces a baseline route
        assert!(matches!(
            plan_catchup(&m, Some(&cursor(2))),
            CatchupPlan::Blocked {
                code: RejectCode::BaselineRequired,
                ..
            }
        ));
    }

    #[test]
    fn a_non_forward_rebaseline_does_not_catch_up_an_installed_client() {
        // A re-baseline at/behind the cursor cannot advance it (RebaselineForward rejects current >= result).
        let m = manifest(5, 2, rebaseline(2), vec![entry(2, 3), entry(4, 5)]); // gap; rebaseline seq == cursor
        assert!(matches!(
            plan_catchup(&m, Some(&cursor(2))),
            CatchupPlan::Blocked {
                code: RejectCode::BaselineRequired,
                ..
            }
        ));
    }

    #[test]
    fn a_client_too_old_for_a_chain_entry_is_blocked() {
        let mut newer = entry(3, 4);
        newer.minimum_client_version = Version::new(9, 9, 9);
        let m = manifest(5, 2, baseline(1), vec![entry(2, 3), newer, entry(4, 5)]);
        assert!(matches!(
            plan_catchup(&m, Some(&cursor(2))),
            CatchupPlan::Blocked {
                code: RejectCode::ClientTooOld,
                ..
            }
        ));
    }

    #[test]
    fn a_schema_ahead_chain_with_a_schema_ahead_baseline_is_blocked() {
        let mut ahead = entry(3, 4);
        ahead.schema_version = CURRENT_SCHEMA_VERSION + 1;
        let mut base = baseline(1);
        base.schema_version = CURRENT_SCHEMA_VERSION + 1; // a too-new baseline cannot fix a too-new schema
        let m = manifest(5, 2, base, vec![entry(2, 3), ahead, entry(4, 5)]);
        assert!(matches!(
            plan_catchup(&m, Some(&cursor(2))),
            CatchupPlan::Blocked {
                code: RejectCode::SchemaAhead,
                ..
            }
        ));
    }

    #[test]
    fn a_size_ratio_over_budget_flips_the_decision_with_no_client_change() {
        // A FORWARD re-baseline is available, so the policy flip can actually route to baseline.
        let mut m = manifest(
            5,
            2,
            rebaseline(5),
            vec![entry(2, 3), entry(3, 4), entry(4, 5)],
        );
        assert!(matches!(
            plan_catchup(&m, Some(&cursor(2))),
            CatchupPlan::Incremental(_)
        ));
        // Chain apply-seconds sum = 15; drop the budget below it (data-driven, no client rebuild).
        m.catchup_policy.max_apply_seconds_budget = 10;
        assert!(matches!(
            plan_catchup(&m, Some(&cursor(2))),
            CatchupPlan::FreshBaseline(_)
        ));
    }

    #[test]
    fn a_bounded_requires_baseline_range_is_authoritative() {
        let mut m = manifest(
            5,
            2,
            rebaseline(5),
            vec![entry(2, 3), entry(3, 4), entry(4, 5)],
        );
        m.catchup_ranges = vec![CatchupRange {
            from_sequence: PackageSequence::new(2),
            to_sequence: Some(PackageSequence::new(3)),
            mode: CatchupMode::RequiresBaseline,
            baseline_id: Some("core-base".to_owned()),
        }];
        assert!(matches!(
            plan_catchup(&m, Some(&cursor(2))),
            CatchupPlan::FreshBaseline(_)
        ));
    }

    #[test]
    fn a_malformed_manifest_with_baseline_ahead_of_head_is_blocked() {
        let m = manifest(5, 2, baseline(9), vec![entry(2, 3)]); // baseline 9 > head 5
        assert!(matches!(
            plan_catchup(&m, Some(&cursor(2))),
            CatchupPlan::Blocked {
                code: RejectCode::WrongGeneration,
                ..
            }
        ));
    }
}
