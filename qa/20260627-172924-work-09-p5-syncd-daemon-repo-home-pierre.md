# Verdict: GO with adjustments

The P5 direction is right: make the daemon a policy loop over the existing one-shot substrate, not a rewrite of apply. The apply path already owns the hard invariants: signature/digest/entitlement checks, cursor guards, per-corpus apply serialization, activation, readiness stamping, and read-role visibility. P5 should add scheduling, source/trust/clock seams, classification, logging, shutdown, and a daemon-lifetime writer lock.

The adjustments below are about keeping those responsibilities separated and making retry behavior explicit rather than inferred from error strings.

1. **Q1 — Source / Manifest / Trust Seams**

   Do not make the daemon read `manifest.json` inline the way the one-shot `update` currently does. The daemon policy should depend on a source seam for both manifest and artifact access.

   I would avoid mutating `run_catchup`'s existing `CatchupSource` contract. It is correctly narrow: once a plan exists, it fetches only planned artifacts. Add a daemon-level source seam above it:

   ```rust
   pub trait ManifestSource {
       fn fetch_manifest(&self, corpus: &str) -> Result<Signed<RemoteManifest>, SyncError>;
   }

   pub trait PackageSource: ManifestSource + CatchupSource {}
   ```

   Or equivalently, add `latest_manifest` on a new `PackageSource` trait and implement both `PackageSource` and `CatchupSource` for `DirectoryCatchupSource`. Keep `run_catchup` taking `&dyn CatchupSource`; planning takes the wider daemon seam.

   This gives the future HTTP/TLS source one clean interface without forcing artifact-only tests to construct a manifest source. It also lets the one-shot `update` share a helper like `fetch_verify_manifest(source, verifier, corpus)` instead of keeping a bespoke `fs::read`.

   Reload the package verifier every cycle, or at least every corpus cycle. `load_package_verifier(conn)` reads installed package trust anchors from the DB; loading it once at startup would miss key rotation or operator-installed anchors. The cost is small compared with artifact apply, and the daemon is long-lived. License entitlement is already checked inside apply and reloads its relevant verifier/token data there.

2. **Q2 — Error Classification / Retry Backoff**

   Use an explicit daemon classification layer. A useful shape is:

   ```rust
   enum CycleOutcome {
       Progress,
       UpToDate,
       Retryable { reason: String },
       Rejected { code: RejectCode, message: String },
       Fatal { message: String },
   }
   ```

   Suggested taxonomy:

   - **Progress:** `BaselineApplied` or `IncrementalApplied { applied > 0 }`. Reset backoff and immediately re-plan that corpus, or short-poll. A fresh baseline may not be the remote head, so an immediate re-plan can discover the retained incremental tail.
   - **Up-to-date:** sleep the normal interval.
   - **Retryable transient:** source fetch/IO blips, DB connection outages, and explicit apply/switch lock contention. Use exponential backoff with a cap and reset it after progress.
   - **Rejected:** `SyncError::Reject { code, .. }` from signed contract/precondition failures: bad signature, digest mismatch, missing entitlement, sequence gap, schema ahead, client too old, wrong feed, etc. Cursor is unchanged. Log loudly, mark health degraded for that corpus, and keep polling on the normal interval or a slower reject interval. Do not exit just because the producer/operator must fix something.
   - **Fatal:** invalid daemon configuration, impossible startup wiring, or losing the daemon-lifetime lock connection. These are process-level failures, not feed/package outcomes.

   Lock timeout is not the only retryable class, but it is the only retryable class currently hidden behind apply internals. The existing `SyncError` / `RejectCode` surface is not enough to classify it robustly. `RejectCode::WrongGeneration` can mean a real cursor/generation conflict, but the incremental lock-busy path currently also returns `WrongGeneration` with `"another apply holds the advisory lock"`. Baseline activation lock contention can surface as `StorageError::Generations` with a similar message.

   Add an explicit predicate or variant rather than string-matching:

   - best: add `SyncError::Retryable { kind: RetryableKind, message: String }` or `SyncError::LockBusy { message: String }`, and map the known lock-busy sites to it;
   - acceptable short cut: add `SyncError::is_retryable()` and update the lock-busy sites to be distinguishable without parsing arbitrary text.

   Persistent rejects should keep polling. The architecture text says warn-and-reject logs and retries on the next tick, and that is the right operational behavior: a missing entitlement, bad manifest, or producer gap can self-heal after an operator installs a token, rotates a key, or republishes the feed. Avoid a systemd restart loop for data-plane rejects.

3. **Q3 — Clock / Shutdown Testability**

   Use a shutdown token that can interrupt sleep. An `Arc<AtomicBool>` checked only between cycles is easy to test, but it makes SIGTERM during a long poll interval wait until the full sleep expires unless your `Clock` seam polls internally.

   A better shape is a small abstraction like:

   ```rust
   trait Clock {
       fn now(&self) -> Instant;
       fn wait_or_shutdown(&self, duration: Duration, shutdown: &ShutdownToken) -> bool;
   }
   ```

   where `true` means shutdown was requested. The production token can be an `AtomicBool + Condvar` or an `mpsc::Receiver<()>` used with `recv_timeout`. The tests can use a fake clock that records requested sleeps and returns immediately.

   Do not interrupt an in-flight apply. That guarantee is correct. The safe shutdown points are before starting a corpus cycle, after a corpus finishes, and during sleeps. If SIGTERM arrives while apply is in progress, finish that apply attempt and then stop before starting the next corpus. That is more responsive than requiring the whole multi-corpus cycle to finish and still preserves the transaction/cursor invariants.

4. **Q4 — Single Writer Lock**

   Add a daemon-lifetime session advisory lock on a dedicated writer connection. The existing locks are necessary but not sufficient for the daemon ownership invariant:

   - the per-corpus apply lock serializes applies for one corpus;
   - the short transaction switch lock serializes activation/switch work;
   - cursor guards prevent corruption if two writers race.

   That is enough for correctness, but it still allows two daemons to duplicate planning/fetching and interleave between applies. P5 wants the daemon to be the only writer. A dedicated session lock makes that true at startup.

   Use a new lock key, not `APPLY_ADVISORY_LOCK_KEY` and not `CORPUS_APPLY_LOCK_BASE`. For the current target, make it global per database/site, because one daemon owns the configured multi-corpus set:

   ```rust
   pub const SYNCD_DAEMON_LOCK_KEY: i64 = 0x6a75_7269_7333; // "juris3"
   SELECT pg_advisory_lock($1);
   ```

   Hold that connection for the daemon lifetime and do not use it for apply work. If the lock-holder connection dies, treat it as fatal and exit; the lock is gone, so the daemon no longer has its single-writer lease.

   Keep the existing per-corpus and switch locks. They still protect one-shot `update`, tests, and accidental writers, and they give apply its local correctness even if the daemon lock is not held.

5. **Q5 — Multi-Corpus / Loop Granularity**

   Yes: one daemon cycle should iterate the configured corpus set, and each corpus should be planned/applied/classified independently. A reject or transient failure for `core` must not stop `cass` from catching up in the same cycle.

   A single configured poll interval for the whole daemon is the right P5 default. Internally, keep per-corpus outcome/backoff state if it is not much extra code, so a transient source failure for one corpus does not unnecessarily slow healthy corpora. If you want the smaller first slice, use one global interval but still continue through the remaining corpora after a per-corpus reject.

   On progress, short-poll or immediately re-run that corpus until it reaches `UpToDate` or hits a bounded burst limit. On `Rejected`, log the closed code and continue polling on interval. On `Retryable`, apply backoff for that corpus and continue with the rest.

## Additional P5 Risks

- **Publish atomicity:** the filesystem source can observe a manifest before all artifact directories are present unless the producer publishes by temp path plus atomic rename. Treat missing artifact IO as retryable, not reject.
- **Structured logs:** include corpus, cursor sequence, manifest head, plan kind, package id/digest when known, reject code, retry class, and next backoff. This is the daemon's operator interface until there is richer health.
- **Trust-anchor rotation:** verifier reload per cycle is the simple safe choice. Startup-only verifier construction makes key rotation operationally surprising.
- **Second-daemon tests:** test the daemon-lifetime lock separately from apply's per-corpus lock. The expected second instance behavior is "blocks at startup," not "races until apply."
- **One-shot compatibility:** leave `update` as a thin one-shot over the same manifest/source helper and `run_catchup`. That is the rollback path and should remain behaviorally unchanged.
