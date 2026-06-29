//! No-infra acceptance gates for M3 Phase 5 automatic rebaseline ROUTING + the boundary GUARD:
//! - a newer DILA baseline than the adopted one is detected as `RebaselinePending`;
//! - the group run-kind routes to `Rebaseline`;
//! - an ordinary incremental path REFUSES (`needs-rebaseline`) to cross the boundary as a delta;
//! - adopting the new baseline clears the pending state (the recorded adoption).
//!
//! This proves the boundary logic without any DB / network — the actual rebaseline PUBLISH + site
//! convergence is exercised by the infra-gated `rebaseline_convergence_gated` test.

use jurisearch_fetch::{ArchiveSource, FetchCursor};
use jurisearch_producer::baseline::{
    AdoptedBaseline, BaselineDecision, RunKind, baseline_decision, decide,
    ensure_incremental_may_proceed, group_run_kind,
};

const OLD_BASELINE: &str = "Freemium_legi_global_20240101-000000.tar.gz";
const NEW_BASELINE: &str = "Freemium_legi_global_20250713-140000.tar.gz";

#[test]
fn decide_is_pure_over_fetched_vs_adopted() {
    assert_eq!(decide(None, None), BaselineDecision::NoBaselineFetched);
    assert_eq!(
        decide(Some(OLD_BASELINE.to_owned()), Some(OLD_BASELINE.to_owned())),
        BaselineDecision::Current {
            baseline_file_name: OLD_BASELINE.to_owned()
        }
    );
    // A newer fetched baseline than the adopted one (or none adopted) is a pending rebaseline.
    assert!(matches!(
        decide(Some(NEW_BASELINE.to_owned()), Some(OLD_BASELINE.to_owned())),
        BaselineDecision::RebaselinePending { .. }
    ));
    assert!(matches!(
        decide(Some(NEW_BASELINE.to_owned()), None),
        BaselineDecision::RebaselinePending { .. }
    ));
}

/// Seed a fetch cursor whose newest baseline is `NEW_BASELINE`, with `OLD_BASELINE` adopted, so a newer
/// upstream baseline is pending.
fn seed_pending(state: &std::path::Path) {
    let mut cursor = FetchCursor::new(ArchiveSource::Legi);
    cursor.baseline_file_name = Some(NEW_BASELINE.to_owned());
    cursor.save(state).unwrap();
    AdoptedBaseline::adopt(state, ArchiveSource::Legi, OLD_BASELINE).unwrap();
}

#[test]
fn a_new_baseline_is_detected_and_routes_to_rebaseline() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    seed_pending(state);

    assert!(matches!(
        baseline_decision(state, ArchiveSource::Legi).unwrap(),
        BaselineDecision::RebaselinePending { .. }
    ));

    let kind = group_run_kind(state, &[ArchiveSource::Legi]).unwrap();
    match kind {
        RunKind::Rebaseline {
            sources_with_new_baseline,
        } => {
            assert_eq!(sources_with_new_baseline.len(), 1);
            assert_eq!(sources_with_new_baseline[0].0, "legi");
            assert_eq!(sources_with_new_baseline[0].1, NEW_BASELINE);
        }
        RunKind::Incremental => panic!("a pending baseline must route to rebaseline"),
    }
}

#[test]
fn an_ordinary_incremental_refuses_to_cross_a_baseline_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    seed_pending(state);

    // The incremental guard must REFUSE with the `needs-rebaseline` class (never silently apply a delta
    // across the new baseline).
    let err = ensure_incremental_may_proceed(state, &[ArchiveSource::Legi]).unwrap_err();
    assert_eq!(err.class(), "needs-rebaseline");
    assert!(err.to_string().contains(NEW_BASELINE), "{err}");
}

#[test]
fn adopting_the_new_baseline_clears_the_pending_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    seed_pending(state);

    // Simulate a completed, recorded rebaseline run adopting the new baseline.
    AdoptedBaseline::adopt(state, ArchiveSource::Legi, NEW_BASELINE).unwrap();

    assert!(matches!(
        baseline_decision(state, ArchiveSource::Legi).unwrap(),
        BaselineDecision::Current { .. }
    ));
    assert_eq!(
        group_run_kind(state, &[ArchiveSource::Legi]).unwrap(),
        RunKind::Incremental
    );
    // The incremental path now proceeds (no pending boundary).
    ensure_incremental_may_proceed(state, &[ArchiveSource::Legi]).unwrap();

    // The adoption is durable + machine-readable.
    let adopted = AdoptedBaseline::load(state, ArchiveSource::Legi).unwrap();
    assert_eq!(adopted.baseline_file_name.as_deref(), Some(NEW_BASELINE));
    assert!(adopted.adopted_at.is_some());
}

/// The under-lock routing RECHECK: `run_update_inner` recomputes the run kind from the adoption markers
/// AFTER acquiring `update-core`. This models two group timers both observing the same pending baseline
/// before either holds the lock: the FIRST run publishes the rebaseline and writes the adoption marker
/// (under the lock); when the SECOND run then acquires the lock and recomputes from the CURRENT markers,
/// the baseline is already adopted, so it routes to an ordinary incremental — NOT a duplicate rebaseline.
#[test]
fn the_second_run_recomputes_to_incremental_once_the_first_run_adopted_the_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();
    seed_pending(state);

    // Both runs would have computed `Rebaseline` from a pre-lock read (the baseline is pending).
    assert!(matches!(
        group_run_kind(state, &[ArchiveSource::Legi]).unwrap(),
        RunKind::Rebaseline { .. }
    ));

    // The FIRST run publishes the rebaseline and records adoption (this is the post-publish marker write
    // that happens UNDER the lock in `run_update_inner`).
    AdoptedBaseline::adopt(state, ArchiveSource::Legi, NEW_BASELINE).unwrap();

    // The SECOND run's recompute UNDER THE LOCK now sees the baseline as adopted: it routes to an
    // ordinary incremental, so no duplicate rebaseline package is built for the same upstream baseline.
    assert_eq!(
        group_run_kind(state, &[ArchiveSource::Legi]).unwrap(),
        RunKind::Incremental,
        "a baseline already adopted under the lock must not trigger a second rebaseline"
    );
    ensure_incremental_may_proceed(state, &[ArchiveSource::Legi]).unwrap();
}
