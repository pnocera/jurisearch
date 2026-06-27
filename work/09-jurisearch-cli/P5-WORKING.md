# P5 working notes — codex design GO-with-adjustments (qa/20260627-172924)

P5 = syncd daemon: a POLICY LOOP over the existing one-shot substrate (load_package_verifier →
fetch+verify Signed<RemoteManifest> → check_manifest_corpus → read_client_cursor → plan_catchup →
run_catchup). Do NOT rewrite apply — it owns the hard invariants (sig/digest/entitlement, cursor guards,
per-corpus apply serialization, activation, readiness stamp, read visibility). P5 adds scheduling, the
source/trust/clock seams, classification, logging, shutdown, and a daemon-lifetime writer lock.

## Binding adjustments

**Q1 — Seams.** Add a daemon-level source seam ABOVE `CatchupSource` (do NOT widen CatchupSource):
```rust
pub trait ManifestSource { fn fetch_manifest(&self, corpus: &str) -> Result<Signed<RemoteManifest>, SyncError>; }
pub trait PackageSource: ManifestSource + CatchupSource {}
```
Impl both for `DirectoryCatchupSource` (fetch_manifest = read+return `<root>/<corpus>/manifest.json`).
`run_catchup` keeps taking `&dyn CatchupSource`; planning takes the wider `&dyn PackageSource`. Factor a
shared `fetch_verify_manifest(source, verifier, corpus)` (read → verify signature → check_manifest_corpus)
used by BOTH the daemon AND the one-shot `update` (leave `update` a thin one-shot over the same helper +
run_catchup — the rollback path, behaviorally unchanged). **RELOAD the package verifier every cycle** (or
every corpus cycle) — catches anchor rotation / operator-installed anchors; cheap vs apply.

**Q2 — Error classification (explicit, NOT string-matching).**
```rust
enum CycleOutcome { Progress, UpToDate, Retryable{reason}, Rejected{code,message}, Fatal{message} }
```
- Progress = BaselineApplied | IncrementalApplied{applied>0} → reset backoff, immediately re-plan that
  corpus (short-poll / bounded burst — a fresh baseline may not be remote head).
- UpToDate → sleep the normal interval.
- Retryable = source fetch/IO blips (incl. publish-atomicity: a manifest seen before its artifacts →
  MISSING ARTIFACT IO IS RETRYABLE, not reject), DB outages, apply/switch lock contention → exponential
  backoff with a cap, reset after progress.
- Rejected = `SyncError::Reject{code}` (bad sig, digest mismatch, missing entitlement, sequence gap,
  schema ahead, client too old, wrong feed) → cursor UNCHANGED, log loudly, mark degraded, KEEP POLLING
  on interval (self-heals when operator fixes it). Do NOT exit / no systemd restart loop for data-plane rejects.
- Fatal = bad daemon config, impossible wiring, or LOST daemon-lock connection → process exit.
CRITICAL: lock-busy is currently hidden behind apply internals AND ambiguous — `RejectCode::WrongGeneration`
is used for BOTH a real cursor/generation conflict AND the incremental lock-busy ("another apply holds the
advisory lock"); baseline activation lock contention surfaces as StorageError::Generations. So ADD an
explicit signal — `SyncError::LockBusy{message}` (or is_retryable()) — and MAP the known lock-busy sites to
it; never parse error text. `is_retryable()` = LockBusy | Storage(AdvisoryLockBusy|StorageLockBusy) | Io.

**Q3 — Clock + shutdown (interruptible).**
```rust
trait Clock { fn now(&self) -> Instant; fn wait_or_shutdown(&self, d: Duration, s: &ShutdownToken) -> bool; }  // true = shutdown requested
```
Production ShutdownToken = AtomicBool + Condvar (or mpsc recv_timeout) so SIGTERM interrupts a long sleep.
Test clock records requested sleeps + returns immediately. NEVER interrupt an in-flight apply: safe stop
points = before a corpus cycle, after a corpus finishes, during sleeps. SIGTERM mid-apply → finish that
apply, then stop before the next corpus (more responsive than finishing the whole multi-corpus cycle).

**Q4 — Daemon-lifetime SESSION lock.** Acquire a session-level `pg_advisory_lock($1)` on a DEDICATED
writer connection, held for the daemon lifetime (NOT used for apply). New GLOBAL key (per database/site —
one daemon owns the configured multi-corpus set), distinct from APPLY/CORPUS locks:
`SYNCD_DAEMON_LOCK_KEY: i64 = 0x6a7572697333` ("juris3"). A 2nd daemon BLOCKS at startup. If the
lock-holder connection dies → FATAL (lease lost) → exit. Keep the existing per-corpus + switch locks.

**Q5 — Multi-corpus / granularity.** One cycle = iterate the configured corpus set, each
planned/applied/classified INDEPENDENTLY (one corpus's reject must not stop another). One global poll
interval (P5 default); keep per-corpus backoff state if cheap. On Progress → short-poll that corpus to
UpToDate or a bounded burst; on Rejected → log + continue; on Retryable → per-corpus backoff + continue.

## Structured logs
Each cycle/outcome: corpus, cursor sequence, manifest head, plan kind, package id/digest (when known),
reject code, retry class, next backoff.

## Tests
catch-up loop offline→head (fake clock + DirectoryCatchupSource); bad/unauthorized package → Rejected +
cursor untouched + keeps polling; lock-timeout → Retryable + retried; 2nd daemon → BLOCKS at startup (test
the daemon-lifetime lock SEPARATELY from apply's per-corpus lock); apply-during-live-read (reuse work/08
soak). systemd unit (.service). Then codex review → commit on main.

## Build order
SyncError retryable signal (+ map lock-busy sites) → ManifestSource/PackageSource + fetch_verify_manifest
→ Clock/ShutdownToken → run_daemon loop + classification → `run` subcommand + SIGTERM/SIGINT (libc) +
session lock → tests → systemd unit.
