## Findings

### BLOCKER: SIGTERM during a progress apply can still start the next apply

`run_daemon` checks `shutdown.is_requested()` before each corpus, but not before each burst iteration. If SIGTERM/SIGINT arrives while `run_corpus_cycle` is applying a baseline or incremental, the signal thread sets the shutdown token; when the apply returns `CycleOutcome::Progress`, the `continue` at `crates/jurisearch-syncd/src/daemon.rs:220` immediately starts the next `run_corpus_cycle` for the same corpus without observing shutdown (`crates/jurisearch-syncd/src/daemon.rs:213-220`). That violates the P5 shutdown contract from the design note: finish the in-flight apply, then stop before beginning more work. In the common offline-to-head path this can continue through up to `max_burst` additional applies after shutdown, which is exactly the case where graceful stop may exceed the systemd stop window.

The current tests do not catch this because `StopAfterOneCycle` requests shutdown only from `wait_or_shutdown`, after the whole burst has already drained. Add a shutdown check at the top of every burst iteration or immediately after a progress outcome before continuing, and add a fake source/clock test where shutdown is requested during a progress cycle and the second planned apply is not started.

### BLOCKER: the systemd unit ignores the configured artifact-root hardening path

`deploy/systemd/jurisearch-syncd.service:49` sets:

```ini
ReadOnlyPaths=${JURISEARCH_SOURCE_ROOT}
```

`systemd-analyze verify deploy/systemd/jurisearch-syncd.service` reports `ReadOnlyPaths= path is not absolute, ignoring: ${JURISEARCH_SOURCE_ROOT}`. In other words, the EnvironmentFile value is not expanded for that directive, so the unit does not actually apply the claimed source-root read-only allowlist. This makes the deployed unit materially different from the P5 operational contract: the comment says the daemon has DB connectivity plus read access to the published artifact root only, but systemd discards that path. If the source root is under a protected location such as a home directory, it can also be unavailable despite being set in `syncd.env`.

Use a literal absolute path in the shipped unit/drop-in, generate the unit from the env file, or document a required override such as `ReadOnlyPaths=/srv/jurisearch/packages` instead of relying on `${JURISEARCH_SOURCE_ROOT}` expansion in this sandboxing directive.

### LOW: the staged diff fails whitespace checking

`git diff --cached --check` fails on `qa/20260627-172924-work-09-p5-syncd-daemon-repo-home-pierre.md:111` with `new blank line at EOF`. This is not a daemon behavior issue, but it leaves the staged change failing the repository's basic diff hygiene check.

## What Looks Sound

The main daemon shape matches the approved P5 direction: `ManifestSource`/`PackageSource` sits above the existing artifact-only `CatchupSource`, `fetch_verify_manifest` is shared by `update` and the daemon, the package verifier is reloaded per corpus cycle, lock-busy contention now has a typed retryable signal, and the daemon lease uses a dedicated session advisory lock.

The focused acceptance test proves the happy-path daemon composition over the real planner/apply substrate: a fresh client converges from offline to sequence 3 through baseline plus retained incrementals using `DirectoryCatchupSource`.

## Tests Run

```text
cargo test -p jurisearch-syncd
cargo test -p jurisearch-package-build --test daemon_loop
cargo test -p jurisearch-package-build --test conformance_reject_codes
cargo test -p jurisearch-package-build --test daemon_loop the_daemon_converges_a_fresh_client_offline_to_head_in_one_cycle
cargo fmt --check
```

All of the above passed.

```text
systemd-analyze verify deploy/systemd/jurisearch-syncd.service
```

Failed as expected in this checkout because `/usr/local/bin/jurisearch-syncd` is not installed here, but it also surfaced the real unit-file issue: `ReadOnlyPaths= path is not absolute, ignoring: ${JURISEARCH_SOURCE_ROOT}`.

```text
git diff --cached --check
```

Failed with `qa/20260627-172924-work-09-p5-syncd-daemon-repo-home-pierre.md:111: new blank line at EOF`.

VERDICT: FIXES_REQUIRED
