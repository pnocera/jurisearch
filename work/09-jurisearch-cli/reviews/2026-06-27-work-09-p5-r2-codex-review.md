## Findings

### BLOCKER: lease loss during a progress burst can still start more applies

`run_daemon` pings the daemon-lifetime lease only once at the top of the outer cycle (`crates/jurisearch-syncd/src/daemon.rs:197`), but a corpus that returns `CycleOutcome::Progress` immediately continues into the next burst iteration (`crates/jurisearch-syncd/src/daemon.rs:213-227`). The new shutdown check at the top of each burst correctly prevents a SIGTERM-during-apply from starting the next apply, but the same protection was not added for the daemon single-writer lease.

That leaves a real single-writer hole: if the dedicated lock connection from `main` (`crates/jurisearch-syncd/src/main.rs:300-321`) dies while burst 0 is applying a baseline, Postgres releases the session advisory lock. A second daemon can then acquire the lease. The first daemon returns `Progress` and starts burst 1 without calling `lock_alive()` again, so it can perform additional manifest fetches/applies after it is no longer the exclusive writer. In the fresh-client path this can continue through the retained incremental tail, up to `max_burst`, before the next outer-loop lease ping. That violates the P5 contract and the code comment that a dead lock connection means the daemon is no longer the exclusive writer and must exit fatal.

Fix by checking the lease before every `run_corpus_cycle` start, at the same safe point as the new shutdown check, or by factoring a shared "pre-work guard" that observes both shutdown and lease loss before every corpus/burst apply. Add a regression test analogous to `shutdown_during_a_progress_burst_does_not_start_the_next_apply`: make `lock_alive` return `true` for the initial cycle check and `false` before the second burst, then assert the second manifest fetch/apply is not started and `run_daemon` exits with an error.

## What Looks Sound

The previous shutdown blocker is addressed in the daemon loop: `run_daemon` now checks `shutdown.is_requested()` at the top of every burst iteration before `run_corpus_cycle` starts (`crates/jurisearch-syncd/src/daemon.rs:213-221`). The new `shutdown_during_a_progress_burst_does_not_start_the_next_apply` test drives shutdown during the first manifest fetch and proves the second burst's manifest fetch is not reached (`crates/jurisearch-package-build/tests/daemon_loop.rs:413-523`).

The systemd unit no longer relies on environment expansion in `ReadOnlyPaths`; it ships a literal absolute path and documents the required drop-in/inline override when deployments use a different `JURISEARCH_SOURCE_ROOT` (`deploy/systemd/jurisearch-syncd.service:49-53`). `git diff --cached --check` is also clean.

The main P5 composition remains aligned with the working notes: the daemon reuses `fetch_verify_manifest`, reloads the package verifier per corpus cycle, classifies retryable lock contention explicitly, keeps data-plane rejects non-fatal, and holds the daemon lease on a dedicated connection.

## Tests Run

```text
cargo test -p jurisearch-syncd
cargo test -p jurisearch-package-build --test daemon_loop
cargo test -p jurisearch-package-build --test conformance_reject_codes
cargo fmt --check
git diff --cached --check
```

All passed.

```text
systemd-analyze verify deploy/systemd/jurisearch-syncd.service
```

This still failed in this checkout because `/usr/local/bin/jurisearch-syncd` is not installed. It also printed an unrelated host warning about `flatpak-add-fedora-repos.service` being executable. It did not report the old `ReadOnlyPaths=${JURISEARCH_SOURCE_ROOT}` expansion problem.

VERDICT: FIXES_REQUIRED
