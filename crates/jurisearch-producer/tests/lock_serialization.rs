//! No-infra acceptance gates for M3 Phase 3 mutual exclusion on the single `update-core` lock:
//! - the legislation and jurisprudence timers cannot hold the DB-mutating lock concurrently;
//! - a closely-spaced run waits (bounded) and then proceeds AFTER the holder — it is never lost;
//! - a held jurisprudence run (modelling ingest→…→publish) blocks a concurrent legislation publish from
//!   shipping until the jurisprudence run completes (the single lock prevents an interleaved publish);
//! - a wait that times out surfaces the distinct `skipped-lock-held` signal (not a silent no-op).

use std::sync::{Arc, Barrier, Mutex};
use std::thread;
use std::time::Duration;

use jurisearch_producer::lock::{acquire_update_lock, is_update_lock_held};

#[test]
fn a_timed_out_wait_is_a_classified_skip_not_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let held = acquire_update_lock(dir.path(), Duration::from_secs(1)).unwrap();
    assert!(is_update_lock_held(dir.path()), "probe sees the held lock");

    let err = acquire_update_lock(dir.path(), Duration::from_millis(200)).unwrap_err();
    assert_eq!(err.class(), "skipped-lock-held");

    drop(held);
    // Once released, the probe is clear and a fresh acquire succeeds (the work is not lost on the next run).
    assert!(!is_update_lock_held(dir.path()));
    let _again = acquire_update_lock(dir.path(), Duration::from_secs(1)).unwrap();
}

#[test]
fn a_held_jurisprudence_run_blocks_a_concurrent_legislation_publish_until_it_completes() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_path_buf();
    // The shared "publish order" log — proves no interleaving of the two groups' publishes.
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    // Gate the legislation thread to start only AFTER jurisprudence holds the lock.
    let lock_held = Arc::new(Barrier::new(2));

    let juri = {
        let state = state.clone();
        let order = Arc::clone(&order);
        let lock_held = Arc::clone(&lock_held);
        thread::spawn(move || {
            // Jurisprudence acquires the single core lock and holds it across ingest → … → publish.
            let guard = acquire_update_lock(&state, Duration::from_secs(5)).unwrap();
            lock_held.wait(); // release legislation to start contending
            order.lock().unwrap().push("juri-ingest");
            thread::sleep(Duration::from_millis(300)); // simulate enrich/embed before publish
            order.lock().unwrap().push("juri-publish");
            drop(guard);
        })
    };

    let legi = {
        let state = state.clone();
        let order = Arc::clone(&order);
        let lock_held = Arc::clone(&lock_held);
        thread::spawn(move || {
            lock_held.wait(); // jurisprudence now holds the lock
            // A generous bounded wait: legislation must WAIT for the holder, then proceed (work not lost).
            let guard = acquire_update_lock(&state, Duration::from_secs(5)).unwrap();
            order.lock().unwrap().push("legi-publish");
            drop(guard);
        })
    };

    juri.join().unwrap();
    legi.join().unwrap();

    let order = order.lock().unwrap();
    // The single lock forces the WHOLE jurisprudence span (ingest→publish) to complete before the
    // legislation publish — never an interleaved publish of half-processed scopes.
    assert_eq!(
        &*order,
        &["juri-ingest", "juri-publish", "legi-publish"],
        "the single update-core lock must serialize the two groups' DB-mutating spans"
    );
}
