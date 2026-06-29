//! No-infra acceptance gates for M3 Phase 4 observability: durable run records round-trip, the
//! current/stale/broken classification is exhaustive, and `build_status` populates a clear schema from
//! on-disk state alone (no DB, no network, no logs).

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use jurisearch_package::compat::Version;
use jurisearch_package::corpus::Corpus;
use jurisearch_package::crypto::{KeyEpoch, KeyId, Signature};
use jurisearch_package::manifest::RemoteManifest;
use jurisearch_package::manifest::remote::{
    BaselineRef, CatchupPolicy, EntitlementListing, EntitlementTier, SigningInfo,
};
use jurisearch_package::package_kind::PackageKind;
use jurisearch_package::sequence::PackageSequence;
use jurisearch_package::signed::Signed;
use jurisearch_package_build::published_manifest_path;
use jurisearch_producer::PRODUCER_CONFIG_EXAMPLE;
use jurisearch_producer::baseline::AdoptedBaseline;
use jurisearch_producer::config::ProducerConfig;
use jurisearch_producer::runrecord::{RunKindTag, RunOutcome, RunRecord};
use jurisearch_producer::status::{OverallState, build_status, build_status_at, is_stale_by_age};
use jurisearch_producer::timestamp::{rfc3339_from_unix, unix_from_rfc3339};

/// Write a minimal signed `core` remote manifest into the served root so `status` classifies the host as
/// HEALTHY (a published head exists) — the precondition for `OverallState` to resolve `Stale` rather than
/// `Broken`. Only the fields `status` reads (`head_sequence`, `generated_at`, `active_baseline`) need to
/// be meaningful; the rest are inert placeholders (the manifest is not verified here).
fn publish_minimal_manifest(corpora_dir: &Path) {
    fn stub_sig() -> Signature {
        Signature {
            algorithm: "stub".to_owned(),
            key_id: KeyId("k".to_owned()),
            key_epoch: KeyEpoch(0),
            signature_hex: String::new(),
        }
    }
    let manifest = RemoteManifest {
        manifest_version: 1,
        generated_at: "2026-06-29T00:00:00Z".to_owned(),
        publisher: "jurisearch".to_owned(),
        corpus: Corpus::new("core").unwrap(),
        environment: "test".to_owned(),
        head_sequence: PackageSequence::new(1),
        min_available_sequence: PackageSequence::new(1),
        active_baseline: BaselineRef {
            baseline_id: "core-2026-06-29-g0001".to_owned(),
            generation: "core_g0001".to_owned(),
            package_kind: PackageKind::Baseline,
            sequence: PackageSequence::new(1),
            schema_version: 18,
            minimum_client_version: Version::new(0, 1, 0),
            artifact_uri: "media://core-baseline".to_owned(),
            compressed_size_bytes: 0,
            uncompressed_size_bytes: 0,
            estimated_load_seconds: 0,
            sha256: "sha256:00".to_owned(),
            signature: stub_sig(),
        },
        packages: Vec::new(),
        catchup_ranges: Vec::new(),
        catchup_policy: CatchupPolicy {
            max_incremental_packages: 120,
            max_cumulative_diff_to_baseline_permille: 330,
            max_cumulative_uncompressed_to_baseline_permille: 500,
            max_apply_seconds_budget: 2700,
        },
        entitlement: EntitlementListing {
            corpus: Corpus::new("core").unwrap(),
            tier: EntitlementTier::Open,
            license_epoch: 0,
            audience: None,
        },
        signing: SigningInfo {
            key_id: KeyId("k".to_owned()),
            algorithm: "stub".to_owned(),
        },
    };
    let signed = Signed {
        payload: manifest,
        signature: stub_sig(),
    };
    let path = published_manifest_path(corpora_dir, "core");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(&signed).unwrap()).unwrap();
}

/// Persist a SUCCESSFUL, recently-ended run for `group` so the group is neither never-ran nor stale by
/// age at `now_unix` (used to remove the `any_never_ran` masking the NIT flagged).
fn seed_recent_success(state: &Path, group: &str, sources: &[String], ended_unix: u64) {
    let mut ok = RunRecord::started(
        group,
        &format!("{group}-recent"),
        sources,
        RunKindTag::Incremental,
    );
    ok.finish("published", None);
    ok.ended_at = Some(rfc3339_from_unix(ended_unix));
    ok.save(state).unwrap();
}

fn config_under(root: &Path) -> ProducerConfig {
    let secrets = root.join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    for name in ["postgres-admin-password", "jurisearch-write-password"] {
        let p = secrets.join(name);
        std::fs::write(&p, "x").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let seed = secrets.join("producer-signing.seed");
    std::fs::write(&seed, "00".repeat(32)).unwrap();
    std::fs::set_permissions(&seed, std::fs::Permissions::from_mode(0o600)).unwrap();
    let toml = PRODUCER_CONFIG_EXAMPLE
        .replace("/etc/jurisearch/secrets", secrets.to_str().unwrap())
        .replace(
            "/srv/jurisearch/storebox/packages",
            root.join("packages").to_str().unwrap(),
        )
        .replace(
            "/srv/jurisearch/storebox/archives",
            root.join("archives").to_str().unwrap(),
        )
        .replace(
            "/var/lib/jurisearch-producer",
            root.join("state").to_str().unwrap(),
        );
    let config = ProducerConfig::parse_str(&toml, Path::new("producer.toml")).unwrap();
    config.validate().unwrap();
    config
}

#[test]
fn run_record_round_trips_and_updates_the_last_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    let mut record = RunRecord::started(
        "legislation",
        "legislation-100",
        &["legi".to_owned()],
        RunKindTag::Incremental,
    );
    assert_eq!(record.outcome, RunOutcome::Running);
    record.published_package = Some("core-1-2".to_owned());
    record.finish("published", None);
    record.save(state).unwrap();

    // Round-trips by run id.
    let loaded = RunRecord::load(state, "legislation", "legislation-100")
        .unwrap()
        .unwrap();
    assert_eq!(loaded, record);
    assert_eq!(loaded.outcome, RunOutcome::Success);
    assert_eq!(loaded.exit_class, "published");

    // The `last.json` pointer resolves to the newest record without scanning the directory.
    let last = RunRecord::load_last(state, "legislation").unwrap().unwrap();
    assert_eq!(last.run_id, "legislation-100");

    // A failure record finishes as a Failure with the error captured.
    let mut bad = RunRecord::started(
        "legislation",
        "legislation-101",
        &[],
        RunKindTag::Incremental,
    );
    bad.finish("publish-failed", Some("disk full".to_owned()));
    bad.save(state).unwrap();
    let last = RunRecord::load_last(state, "legislation").unwrap().unwrap();
    assert_eq!(last.outcome, RunOutcome::Failure);
    assert_eq!(last.error.as_deref(), Some("disk full"));
}

#[test]
fn overall_classification_is_exhaustive_over_current_stale_broken() {
    // A failed run is broken regardless of the manifest.
    assert_eq!(
        OverallState::classify(true, true, false),
        OverallState::Broken
    );
    // No published manifest yet is broken.
    assert_eq!(
        OverallState::classify(false, false, false),
        OverallState::Broken
    );
    // Healthy + a pending baseline / in-flight run / never-ran group is stale (behind upstream).
    assert_eq!(
        OverallState::classify(false, true, true),
        OverallState::Stale
    );
    // Healthy, settled, nothing pending is current.
    assert_eq!(
        OverallState::classify(false, true, false),
        OverallState::Current
    );
}

#[test]
fn status_reports_broken_with_no_runs_and_surfaces_a_failed_run_and_pending_baseline() {
    let root = tempfile::tempdir().unwrap();
    let config = config_under(root.path());
    let state = &config.producer.state_dir;

    // Fresh host: no manifest published ⇒ broken; both groups present with no last run.
    let status = build_status(&config).unwrap();
    assert_eq!(status.overall, OverallState::Broken);
    assert_eq!(status.groups.len(), 2);
    assert!(status.published_head_sequence.is_none());
    let legi = status
        .groups
        .iter()
        .find(|g| g.group == "legislation")
        .unwrap();
    assert!(legi.last_run_id.is_none());
    assert!(!legi.rebaseline_pending);

    // Record a FAILED jurisprudence run ⇒ status surfaces it (still broken, with the error + class).
    let mut failed = RunRecord::started(
        "jurisprudence",
        "jurisprudence-7",
        &["cass".to_owned()],
        RunKindTag::Incremental,
    );
    failed.finish("publish-failed", Some("ed25519 sign failed".to_owned()));
    failed.save(state).unwrap();
    let status = build_status(&config).unwrap();
    assert_eq!(status.overall, OverallState::Broken);
    let juri = status
        .groups
        .iter()
        .find(|g| g.group == "jurisprudence")
        .unwrap();
    assert_eq!(juri.last_exit_class.as_deref(), Some("publish-failed"));
    assert_eq!(juri.last_outcome, Some(RunOutcome::Failure));
    assert!(juri.last_error.as_deref().unwrap().contains("sign failed"));

    // Adopt an OLD baseline for legi, then write a NEWER fetched baseline into the fetch cursor — the
    // group is now `rebaseline_pending` (a newer DILA baseline awaits adoption).
    use jurisearch_fetch::{ArchiveSource, FetchCursor};
    AdoptedBaseline::adopt(
        state,
        ArchiveSource::Legi,
        "Freemium_legi_global_20240101-000000.tar.gz",
    )
    .unwrap();
    let mut cursor = FetchCursor::new(ArchiveSource::Legi);
    cursor.baseline_file_name = Some("Freemium_legi_global_20250713-140000.tar.gz".to_owned());
    cursor.save(state).unwrap();
    let status = build_status(&config).unwrap();
    let legi = status
        .groups
        .iter()
        .find(|g| g.group == "legislation")
        .unwrap();
    assert!(
        legi.rebaseline_pending,
        "a newer fetched baseline than the adopted one must show as pending"
    );
    let bl = legi.baselines.iter().find(|b| b.source == "legi").unwrap();
    assert_eq!(bl.state, "rebaseline_pending");
}

#[test]
fn a_last_successful_but_old_run_classifies_as_stale_by_age() {
    let root = tempfile::tempdir().unwrap();
    let config = config_under(root.path());
    let state = &config.producer.state_dir;

    // A published manifest makes the host HEALTHY, so the overall state can resolve to `Stale` (not the
    // `Broken` a no-manifest host always reports). No fetch cursor / adoption marker is written, so no
    // baseline is pending — `stale-by-age` is the ONLY `behind` signal in play.
    publish_minimal_manifest(&config.producer.corpora_dir);

    // The NIT (Codex r2): seed a NON-stale successful run for EVERY group first, so `any_never_ran` is
    // false and cannot mask the stale signal — then make ONLY jurisprudence old. If `any_stale` were
    // dropped from `OverallState::classify`'s `behind` input, the overall state would fall back to
    // `Current` and this test would FAIL (which is the point).
    let ended_unix = 1_700_000_000; // 2023-11-14T...
    let now = ended_unix + 30 * 86_400;
    // legislation: a recent successful run (well within its daily cadence budget at `now`).
    seed_recent_success(state, "legislation", &["legi".to_owned()], now - 60);

    // jurisprudence: a SUCCESSFUL run whose terminal record ended days ago — stalled purely by age.
    let mut ok = RunRecord::started(
        "jurisprudence",
        "jurisprudence-1",
        &["cass".to_owned()],
        RunKindTag::Incremental,
    );
    ok.finish("published", None);
    ok.ended_at = Some(rfc3339_from_unix(ended_unix));
    ok.save(state).unwrap();
    assert_eq!(ok.outcome, RunOutcome::Success);

    // `now` is well beyond the jurisprudence group's daily cadence budget (2 days): the group is stale.
    let status = build_status_at(&config, now).unwrap();
    let juri = status
        .groups
        .iter()
        .find(|g| g.group == "jurisprudence")
        .unwrap();
    assert_eq!(juri.last_outcome, Some(RunOutcome::Success));
    assert!(
        juri.stale_by_age,
        "a last-successful run older than the cadence budget must be stale by age"
    );
    // legislation is fresh — so it is NOT the source of staleness (no never-ran masking).
    let legi = status
        .groups
        .iter()
        .find(|g| g.group == "legislation")
        .unwrap();
    assert_eq!(legi.last_outcome, Some(RunOutcome::Success));
    assert!(
        !legi.stale_by_age,
        "the recent legislation run is not stale by age"
    );
    assert!(!legi.rebaseline_pending && !juri.rebaseline_pending);
    // The overall state is STALE *specifically* because of the old successful jurisprudence run: healthy
    // manifest, no failure, no pending baseline, no never-ran group — `any_stale` is the only `behind`.
    assert_eq!(
        status.overall,
        OverallState::Stale,
        "an old successful run with everything else current must classify overall as Stale"
    );

    // A `now` just after the run is NOT stale (the budget has not elapsed).
    let fresh = build_status_at(&config, ended_unix + 60).unwrap();
    let juri_fresh = fresh
        .groups
        .iter()
        .find(|g| g.group == "jurisprudence")
        .unwrap();
    assert!(
        !juri_fresh.stale_by_age,
        "a just-finished successful run is not stale"
    );

    // The pure classifier: only a SUCCESSFUL, sufficiently-old record is stale by age.
    let stale_after = 2 * 86_400;
    assert!(is_stale_by_age(Some(&ok), stale_after, now));
    assert!(!is_stale_by_age(Some(&ok), stale_after, ended_unix + 60));
    assert!(!is_stale_by_age(None, stale_after, now));
    let mut failed = ok.clone();
    failed.finish("publish-failed", Some("x".to_owned()));
    failed.ended_at = Some(rfc3339_from_unix(ended_unix));
    assert!(
        !is_stale_by_age(Some(&failed), stale_after, now),
        "a FAILED run is handled by the failure signal, not stale-by-age"
    );
    assert_eq!(
        unix_from_rfc3339(&rfc3339_from_unix(ended_unix)),
        Some(ended_unix)
    );
}
