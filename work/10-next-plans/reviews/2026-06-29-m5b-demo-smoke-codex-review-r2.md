# M5-B demo/smoke/watchdog re-review r2

## Findings

### BLOCKER: the operated acceptance script cannot run its Tier-1 decision tests

`crates/jurisearch-deploy/scripts/single-host-demo-acceptance.sh:43` invokes:

```sh
cargo test -p jurisearch-deploy --quiet \
  ops::smoke:: ops::watchdog:: ops::demo:: ops::fixture::
```

Cargo accepts only one positional test filter. Running the exact command fails before any test executes with:

```text
error: unexpected argument 'ops::watchdog::' found
```

Impact: the advertised always-on Tier-1 acceptance gate reports `[FAIL] decision-logic unit tests` even when the individual module tests pass. In authorized mode the script keeps running with `RC=1`, so a complete live acceptance run still exits red because the test command syntax is invalid.

Fix: run separate `cargo test` invocations, use a single filter that covers all intended tests, or move these acceptance tests under one common module/filter.

### WARN: negative smoke classifiers still pass malformed successful response shapes

The missing-id contract says the leg should pass only on an empty `documents` array or served `NoResults`, but `fetch_returned_nothing` treats a missing or non-array `documents` field as empty:

- `crates/jurisearch-deploy/src/ops/smoke.rs:293`
- `crates/jurisearch-deploy/src/ops/smoke.rs:297`
- `crates/jurisearch-deploy/src/ops/smoke.rs:363`

That means a successful but unrelated/malformed fetch response such as `{}` or `{ "candidates": [] }` passes the negative missing-id leg as "empty documents", even though it is not one of the accepted not-found shapes.

The bad-query leg has the same shape hole. It fails only when `search_has_candidates(result)` is true; because `search_has_candidates` returns false when `candidates` is missing or non-array, `{}` also passes as a "clean empty result":

- `crates/jurisearch-deploy/src/ops/smoke.rs:322`
- `crates/jurisearch-deploy/src/ops/smoke.rs:326`
- `crates/jurisearch-deploy/src/ops/smoke.rs:410`
- `crates/jurisearch-deploy/src/ops/smoke.rs:417`

Impact: the prior broad "any error/unrelated response can pass" problem is narrowed but not closed. These negative legs can still go green on a broken handler that returns `Ok({})` or another non-contract success body.

Fix: require `documents` to exist and be an array with length zero for missing-id, and require `candidates` to exist and be an array with length zero for the bad-query empty-result allowance. Add tests for `{}`, non-array fields, and wrong successful result shapes.

### WARN: `demo up` does not fail when its synchronous catch-up remains not green

`demo up` now calls `catch_up_corpus` before the readiness gate, which fixes the previous plain-alias behavior. However, the result is only printed. If `catch_up_corpus` returns `CatchupGreen::NoActiveCorpus` or `CatchupGreen::NotAtHead`, `run_install` continues into readiness and may still start the site:

- `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:488`
- `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:498`
- `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:501`
- `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:510`

This matters because `catch_up_corpus` is a single pass. A fresh client against a manifest whose active baseline is behind `head_sequence` can apply the baseline and return `NotAtHead`; readiness is based on the active topology stamp and does not independently compare the cursor to the verified remote head.

Impact: `demo up` can still proceed after reporting that the fixture corpus is not caught up to the verified head, contrary to the command help/dry-run text that says it catches up to the verified producer head before the gate.

Fix: make non-green demo catch-up an immediate failure, or loop like `site catch-up --wait` until each corpus is green before running readiness.

## Confirmed Fixes / Audit Notes

- The watchdog now parses the normal PostgreSQL `applied_at::text` forms covered by tests, including the space-separated numeric-offset forms, and applies offsets when computing epoch seconds (`crates/jurisearch-deploy/src/ops/watchdog.rs:270`, `crates/jurisearch-deploy/src/ops/watchdog.rs:421`).
- A behind cursor with unknown age now fails closed to `StalledCursor`, not `CatchingUp` (`crates/jurisearch-deploy/src/ops/watchdog.rs:139`, `crates/jurisearch-deploy/src/ops/watchdog.rs:371`).
- Fixture bytes are honestly deferred: only `fixtures/README.md` is present under `crates/jurisearch-deploy/fixtures/`, the README says the live fixture demo is unavailable until generated, and `demo up` / `demo smoke` run `ensure_published_artifacts` before continuing (`crates/jurisearch-deploy/fixtures/README.md:37`, `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:400`, `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:673`).
- The acceptance script skips absent fixture live demo legs with an explicit reason (`crates/jurisearch-deploy/scripts/single-host-demo-acceptance.sh:75`).
- The watchdog live path remains read-only with respect to package application: it fetches/verifies the manifest and reads cursor/status, with no `run_catchup` or apply call in the watchdog path (`crates/jurisearch-deploy/src/ops/watchdog.rs:204`, `crates/jurisearch-deploy/src/ops/watchdog.rs:213`, `crates/jurisearch-deploy/src/ops/watchdog.rs:217`).
- Scope is confined to `Cargo.lock`, `crates/jurisearch-deploy/**`, and docs under `work/09-jurisearch-cli/**`; the thin-client crate itself was not edited.

## Validation

- `cargo test -p jurisearch-deploy ops::smoke:: --quiet` passes: 9 tests.
- `cargo test -p jurisearch-deploy ops::watchdog:: --quiet` passes: 10 tests.
- `cargo test -p jurisearch-deploy ops::demo:: --quiet` passes: 4 tests.
- `cargo test -p jurisearch-deploy ops::fixture:: --quiet` passes: 4 tests.
- The acceptance script's combined Tier-1 command fails before running tests because of the multiple positional filters described above.

VERDICT: FIXES_REQUIRED
