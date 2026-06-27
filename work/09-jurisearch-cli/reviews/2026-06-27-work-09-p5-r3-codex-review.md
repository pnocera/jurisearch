## Findings

No blocking findings.

## What Looks Sound

The prior R2 blocker is addressed in the live daemon loop. `check_before_apply` observes shutdown first and then the daemon lease before allowing work to start (`crates/jurisearch-syncd/src/daemon.rs:137-145`), and `run_daemon` calls that guard inside the corpus burst loop immediately before `run_corpus_cycle` (`crates/jurisearch-syncd/src/daemon.rs:219-240`). Because `run_corpus_cycle` owns the verifier reload, manifest fetch, cursor read, plan, and apply (`crates/jurisearch-syncd/src/daemon.rs:150-159`), the lease is now checked before both the first corpus apply and every subsequent progress-burst apply.

The new regression test matches the failed scenario from the previous review. `lease_loss_during_a_progress_burst_is_fatal_and_stops_before_the_next_apply` makes `lock_alive` return `true` for burst 0 and `false` before burst 1 (`crates/jurisearch-package-build/tests/daemon_loop.rs:686-704`), then asserts `run_daemon` returns an error, the client only reached sequence 1, and the second burst's manifest fetch never ran (`crates/jurisearch-package-build/tests/daemon_loop.rs:707-720`). That proves the old hole, where a lost lease could slip through a `Progress` burst, is covered.

The main daemon wiring still keeps the single-writer lease on a dedicated connection and uses the same connection only for liveness pings (`crates/jurisearch-syncd/src/main.rs:296-321`). The systemd unit keeps the R2 fix for `ReadOnlyPaths` as a literal path rather than relying on environment expansion (`deploy/systemd/jurisearch-syncd.service:49-53`).

One non-blocking cleanup remains: the `run_daemon` doc comment still says `lock_alive` is pinged "once per cycle" (`crates/jurisearch-syncd/src/daemon.rs:198-199`), while the implementation now correctly pings before every apply. This is stale documentation, not a behavior blocker.

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

This still cannot complete in this checkout because `/usr/local/bin/jurisearch-syncd` is not installed, and it also reports the unrelated host warning that `/usr/lib/systemd/system/flatpak-add-fedora-repos.service` is executable. It did not report a syntax problem or the old `ReadOnlyPaths=${JURISEARCH_SOURCE_ROOT}` issue.

VERDICT: GO
