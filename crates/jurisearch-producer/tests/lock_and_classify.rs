//! No-infra gates: the `update-core` lock is mutually exclusive with a bounded wait, and an empty
//! outbox window classifies as a SUCCESSFUL no-op (not a failure).

use std::path::PathBuf;
use std::time::Duration;

use jurisearch_package_build::{EnrichmentMode, ProducerCycleReport};
use jurisearch_producer::lock::acquire_update_lock;
use jurisearch_producer::update::classify_cycle;

#[test]
fn the_update_core_lock_is_mutually_exclusive_with_a_bounded_wait() {
    let dir = tempfile::tempdir().unwrap();
    let held = acquire_update_lock(dir.path(), Duration::from_secs(1)).expect("first acquires");
    assert_eq!(held.name(), "update-core");

    // A second acquire while the first is held times out with the distinct `skipped-lock-held` signal.
    let err = acquire_update_lock(dir.path(), Duration::from_millis(300))
        .expect_err("second must not acquire while held");
    assert_eq!(err.class(), "skipped-lock-held");

    // Once released, a fresh acquire succeeds (the lock is not leaked).
    drop(held);
    let _again = acquire_update_lock(dir.path(), Duration::from_secs(1)).expect("re-acquires");
}

fn report(built: Option<&str>, enrichment: EnrichmentMode) -> ProducerCycleReport {
    ProducerCycleReport {
        corpus: "core".to_owned(),
        built_incremental: built.map(str::to_owned),
        head_sequence: built.map(|_| 2),
        included_change_seq_high: built.map(|_| 24),
        remote_manifest_path: PathBuf::from("/srv/packages/core/manifest.json"),
        enrichment,
    }
}

#[test]
fn an_empty_outbox_window_classifies_as_a_successful_no_op() {
    // built_incremental == None means the window was empty: the manifest still refreshed, exit zero.
    let no_op = report(None, EnrichmentMode::Ran { zones_enriched: 0 });
    assert_eq!(classify_cycle(&no_op), "no-op");
}

#[test]
fn a_built_incremental_classifies_published_and_degraded_honestly() {
    let published = report(Some("core-1-2"), EnrichmentMode::Ran { zones_enriched: 5 });
    assert_eq!(classify_cycle(&published), "published");

    let degraded = report(Some("core-1-2"), EnrichmentMode::SkippedNoCredentials);
    assert_eq!(classify_cycle(&degraded), "published-enrich-degraded");
}
