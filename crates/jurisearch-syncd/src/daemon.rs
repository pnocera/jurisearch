//! work/09 P5 — the syncd DAEMON: a policy loop that COMPOSES the one-shot substrate (verifier reload →
//! [`fetch_verify_manifest`] → [`read_client_cursor`] → [`plan_catchup`] → [`run_catchup`]) into a
//! poll→plan→verify→apply cycle. It adds ONLY scheduling, classification, backoff, structured logging,
//! and graceful shutdown; the apply path remains the authoritative trust/cursor boundary. The daemon is
//! the single writer (a daemon-lifetime session advisory lock, acquired in `main`).
//!
//! Testable by construction: the wait/shutdown is a [`Clock`] seam (a recording test clock never
//! sleeps), and shutdown is a [`ShutdownToken`] driven directly in tests (SIGTERM/SIGINT wiring lives in
//! `main`). Graceful = NEVER interrupt an in-flight apply; shutdown is observed only between corpora and
//! during sleeps.

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use jurisearch_package::RejectCode;
use jurisearch_storage::backend::WriterConnection;
use serde_json::json;

use crate::error::SyncError;
use crate::planner::{PackageSource, fetch_verify_manifest};
use crate::{CatchupReport, load_package_verifier, plan_catchup, read_client_cursor, run_catchup};

/// A cooperative shutdown signal that can INTERRUPT a sleep (so SIGTERM during a long poll interval does
/// not wait out the full interval). `request()` is safe to call from a dedicated signal-handling thread
/// (it is NOT a signal handler itself — see `main`).
#[derive(Debug, Default)]
pub struct ShutdownToken {
    requested: Mutex<bool>,
    changed: Condvar,
}

impl ShutdownToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Request shutdown and wake any thread waiting in [`wait_timeout`](Self::wait_timeout).
    pub fn request(&self) {
        let mut requested = self.requested.lock().expect("shutdown mutex poisoned");
        *requested = true;
        self.changed.notify_all();
    }

    #[must_use]
    pub fn is_requested(&self) -> bool {
        *self.requested.lock().expect("shutdown mutex poisoned")
    }

    /// Wait up to `timeout`, returning EARLY if shutdown is (or becomes) requested. Returns `true` if
    /// shutdown was requested.
    #[must_use]
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let requested = self.requested.lock().expect("shutdown mutex poisoned");
        if *requested {
            return true;
        }
        let (requested, _timed_out) = self
            .changed
            .wait_timeout_while(requested, timeout, |requested| !*requested)
            .expect("shutdown condvar poisoned");
        *requested
    }
}

/// The wait seam, so the loop is testable without real sleeps. Production uses [`SystemClock`]; tests use
/// a recording clock that returns immediately. `wait_or_shutdown` returns `true` if shutdown was
/// requested (so the caller stops).
pub trait Clock {
    fn now(&self) -> Instant;
    fn wait_or_shutdown(&self, duration: Duration, shutdown: &ShutdownToken) -> bool;
}

/// The real clock: an interruptible sleep on the [`ShutdownToken`].
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn wait_or_shutdown(&self, duration: Duration, shutdown: &ShutdownToken) -> bool {
        shutdown.wait_timeout(duration)
    }
}

/// The classified result of one corpus cycle (work/09 P5, codex Q2). Errors are classified by TYPE
/// ([`SyncError::is_retryable`]), never by parsing message text.
#[derive(Debug)]
pub enum CycleOutcome {
    /// A baseline or ≥1 incremental was applied — re-plan this corpus (it may not be at remote head).
    Progress,
    /// Nothing to do — sleep the normal interval.
    UpToDate,
    /// A transient fault (lock contention, fetch/IO blip, DB blip) — back off and retry; cursor unchanged.
    Retryable { reason: String },
    /// A permanent contract refusal (bad signature/digest, missing entitlement, sequence gap, …) — log
    /// loudly, leave the cursor UNCHANGED, and keep polling (self-heals when the operator/producer fixes it).
    Rejected { code: RejectCode, message: String },
}

/// Classify one corpus cycle's `Result` into a [`CycleOutcome`]. A `Reject` is permanent; anything
/// `is_retryable()` (or any other non-reject fault — a malformed manifest, a non-lock DB error) is
/// retried rather than crashing the daemon (codex Q2: do NOT exit for data-plane problems).
fn classify(result: Result<CatchupReport, SyncError>) -> CycleOutcome {
    match result {
        Ok(CatchupReport::UpToDate) => CycleOutcome::UpToDate,
        Ok(CatchupReport::BaselineApplied(_) | CatchupReport::IncrementalApplied { .. }) => {
            CycleOutcome::Progress
        }
        Err(error) if error.is_retryable() => CycleOutcome::Retryable {
            reason: error.to_string(),
        },
        Err(SyncError::Reject { code, message }) => CycleOutcome::Rejected { code, message },
        // Non-reject, non-retryable (malformed manifest JSON, a non-lock DB/storage fault): keep polling
        // with backoff. The only FATAL is the daemon's own lock-lease loss, detected at the loop top.
        Err(error) => CycleOutcome::Retryable {
            reason: error.to_string(),
        },
    }
}

/// The pre-work guard decision (work/09 P5): checked before EVERY apply.
enum PreApply {
    /// Clear to start the next apply.
    Proceed,
    /// A graceful shutdown was requested — stop (the in-flight apply already finished).
    Shutdown,
    /// The single-writer lease was lost — FATAL, exit.
    LeaseLost,
}

/// Observe both shutdown and the single-writer lease before starting an apply, so neither a SIGTERM
/// during an in-flight apply nor a lost lease can begin the next one (a Progress burst would otherwise
/// re-enter without re-checking). Shutdown takes precedence (a graceful stop on the same tick the lease
/// is also gone is still a clean stop).
fn check_before_apply(shutdown: &ShutdownToken, lock_alive: &mut dyn FnMut() -> bool) -> PreApply {
    if shutdown.is_requested() {
        PreApply::Shutdown
    } else if !lock_alive() {
        PreApply::LeaseLost
    } else {
        PreApply::Proceed
    }
}

/// Run ONE catch-up cycle for a corpus: reload the verifier (catches anchor rotation), fetch+verify the
/// manifest, read the cursor, plan, and apply through `run_catchup`. The verifier reload + manifest
/// fetch + apply all run here so a fault anywhere is classified uniformly by the caller.
fn run_corpus_cycle(
    apply: &dyn WriterConnection,
    source: &dyn PackageSource,
    corpus: &str,
) -> Result<CatchupReport, SyncError> {
    let verifier = load_package_verifier(apply)?;
    let manifest = fetch_verify_manifest(source, &verifier, corpus)?;
    let cursor = read_client_cursor(apply, corpus)?;
    let plan = plan_catchup(&manifest, cursor.as_ref());
    run_catchup(apply, source, &verifier, plan)
}

/// Daemon configuration (work/09 P5).
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// The corpora this daemon keeps at head.
    pub corpora: Vec<String>,
    /// The normal poll interval when everything is up to date.
    pub poll_interval: Duration,
    /// The first backoff after a transient fault (doubles each consecutive retryable cycle, capped).
    pub min_backoff: Duration,
    /// The backoff cap.
    pub max_backoff: Duration,
    /// Max consecutive immediate re-plans of one corpus after Progress (a fresh baseline may have a
    /// retained incremental tail). Bounds a runaway burst.
    pub max_burst: u32,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            corpora: Vec::new(),
            poll_interval: Duration::from_secs(30),
            min_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(300),
            max_burst: 16,
        }
    }
}

/// Run the daemon loop until shutdown (graceful → `Ok`) or a FATAL fault (lock-lease loss → `Err`).
///
/// Each cycle iterates EVERY configured corpus INDEPENDENTLY (one corpus's reject/transient fault never
/// stops another). On `Progress` a corpus is re-planned immediately up to `max_burst` times (to drain a
/// retained tail). The post-cycle sleep is the normal `poll_interval`, or an exponential backoff (reset
/// on a clean cycle) when any corpus hit a transient fault. Shutdown and the lease are observed by the
/// pre-work guard before EVERY apply (so a SIGTERM or a lost lease never slips a progress burst) — and
/// shutdown also interrupts the sleep — but NEVER mid-apply.
///
/// `lock_alive` is pinged before every apply (via [`check_before_apply`]): if the daemon-lifetime
/// single-writer lock connection has died, the lease is gone, so the daemon exits FATAL rather than
/// writing without exclusivity.
///
/// # Errors
/// [`SyncError`] only on a FATAL condition (the single-writer lock lease was lost).
pub fn run_daemon(
    apply: &dyn WriterConnection,
    source: &dyn PackageSource,
    clock: &dyn Clock,
    shutdown: &ShutdownToken,
    lock_alive: &mut dyn FnMut() -> bool,
    config: &DaemonConfig,
) -> Result<(), SyncError> {
    let mut backoff = config.min_backoff;
    log_event(&json!({ "event": "daemon_start", "corpora": config.corpora,
        "poll_interval_secs": config.poll_interval.as_secs() }));
    loop {
        let mut any_retryable = false;
        for corpus in &config.corpora {
            // Burst: re-plan a corpus until UpToDate (or the burst cap) so a fresh baseline's retained
            // incremental tail is drained in the same cycle.
            for burst in 0..config.max_burst.max(1) {
                // Pre-work guard BEFORE every apply (corpus AND burst): a graceful SHUTDOWN finishes the
                // in-flight apply but never starts the next; a LOST single-writer lease is FATAL — a dead
                // lock connection means another daemon can write, so we exit rather than apply without
                // exclusivity. Both are checked at the SAME safe point so neither can slip a burst.
                match check_before_apply(shutdown, lock_alive) {
                    PreApply::Proceed => {}
                    PreApply::Shutdown => {
                        log_event(&json!({ "event": "daemon_shutdown" }));
                        return Ok(());
                    }
                    PreApply::LeaseLost => {
                        log_event(
                            &json!({ "event": "daemon_fatal", "reason": "single_writer_lease_lost" }),
                        );
                        return Err(SyncError::reject(
                            RejectCode::WrongGeneration,
                            "syncd daemon single-writer lock lease was lost (lock connection died)",
                        ));
                    }
                }
                let outcome = classify(run_corpus_cycle(apply, source, corpus));
                log_event(
                    &json!({ "event": "corpus_cycle", "corpus": corpus, "burst": burst,
                    "outcome": outcome_label(&outcome), "detail": outcome_detail(&outcome) }),
                );
                match outcome {
                    CycleOutcome::Progress => continue, // re-plan immediately (bounded by max_burst)
                    CycleOutcome::UpToDate => break,
                    CycleOutcome::Retryable { .. } => {
                        any_retryable = true;
                        break;
                    }
                    CycleOutcome::Rejected { .. } => break, // cursor unchanged; keep polling on interval
                }
            }
        }

        let sleep = if any_retryable {
            let current = backoff;
            backoff = (backoff * 2).min(config.max_backoff);
            current
        } else {
            backoff = config.min_backoff;
            config.poll_interval
        };
        log_event(
            &json!({ "event": "cycle_sleep", "seconds": sleep.as_secs_f64(),
            "backing_off": any_retryable }),
        );
        if clock.wait_or_shutdown(sleep, shutdown) {
            log_event(&json!({ "event": "daemon_shutdown" }));
            return Ok(());
        }
    }
}

fn outcome_label(outcome: &CycleOutcome) -> &'static str {
    match outcome {
        CycleOutcome::Progress => "progress",
        CycleOutcome::UpToDate => "up_to_date",
        CycleOutcome::Retryable { .. } => "retryable",
        CycleOutcome::Rejected { .. } => "rejected",
    }
}

fn outcome_detail(outcome: &CycleOutcome) -> serde_json::Value {
    match outcome {
        CycleOutcome::Retryable { reason } => json!({ "reason": reason }),
        CycleOutcome::Rejected { code, message } => {
            json!({ "reject_code": code.as_str(), "message": message })
        }
        _ => serde_json::Value::Null,
    }
}

/// Emit one structured (JSON-line) log event to stderr — the daemon's operator interface (corpus, cursor
/// sequence, outcome, reject code, backoff) until a richer health surface exists.
fn log_event(event: &serde_json::Value) {
    eprintln!("{event}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use jurisearch_storage::runtime::StorageError;

    #[test]
    fn lock_contention_classifies_retryable_a_reject_does_not() {
        // Lock-busy (syncd-level AND storage-level) is retryable; a permanent contract reject is not.
        assert!(SyncError::lock_busy("held").is_retryable());
        assert!(
            SyncError::Storage(StorageError::ApplyLockBusy {
                message: "held".to_owned()
            })
            .is_retryable()
        );
        assert!(SyncError::Io(std::io::Error::other("blip")).is_retryable());
        assert!(!SyncError::reject(RejectCode::MissingEntitlement, "no token").is_retryable());

        // `classify` maps each to the right CycleOutcome (by TYPE, never message text).
        assert!(matches!(
            classify(Err(SyncError::lock_busy("held"))),
            CycleOutcome::Retryable { .. }
        ));
        assert!(matches!(
            classify(Err(SyncError::reject(RejectCode::SignatureInvalid, "bad"))),
            CycleOutcome::Rejected {
                code: RejectCode::SignatureInvalid,
                ..
            }
        ));
        assert!(matches!(
            classify(Ok(CatchupReport::UpToDate)),
            CycleOutcome::UpToDate
        ));
        assert!(matches!(
            classify(Ok(CatchupReport::IncrementalApplied { applied: 2 })),
            CycleOutcome::Progress
        ));
    }

    #[test]
    fn an_already_requested_shutdown_returns_from_wait_immediately() {
        let token = ShutdownToken::new();
        token.request();
        // A long timeout returns at once because shutdown is already requested (interruptible wait).
        assert!(token.wait_timeout(Duration::from_secs(3600)));
        assert!(token.is_requested());
    }
}
