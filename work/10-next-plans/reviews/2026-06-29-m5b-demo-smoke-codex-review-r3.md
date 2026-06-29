## Findings

### BLOCKER: live acceptance can still go green on a watchdog alert

`crates/jurisearch-deploy/scripts/single-host-demo-acceptance.sh:105-107`

The authorized Tier-2 watchdog leg ignores the non-zero exit from `jurisearchctl site watchdog`:

```sh
$CTL site watchdog --config "$CONFIG" || echo "  (watchdog signalled an alert exit; see the line above)"
```

`jurisearchctl site watchdog` deliberately returns non-zero when it detects an alert state (`stalled_cursor` or `ahead_of_head`), so swallowing that exit means the acceptance script can finish with `RESULT: ... RC=0` even when the read-only watchdog detected the exact stalled-cursor condition this milestone is supposed to gate. This contradicts the script's own Tier-2 contract at `crates/jurisearch-deploy/scripts/single-host-demo-acceptance.sh:73` ("Each leg is required to succeed") and leaves a false-green hole in the live watchdog acceptance path.

This should set `RC=1` on a watchdog alert, analogous to the `demo up`, `demo smoke`, and `site smoke` legs.

## Verification Notes

The r2 Tier-1 cargo-test filter issue is fixed in the script structure. `crates/jurisearch-deploy/scripts/single-host-demo-acceptance.sh:44-55` loops over `ops::smoke::`, `ops::watchdog::`, `ops::demo::`, and `ops::fixture::`, invoking `cargo test -p jurisearch-deploy --quiet "$module"` once per positional filter and aggregating any failure into `TIER1_RC=1` / `RC=1`. That addresses the invalid multi-positional `cargo test` gate and preserves failure propagation for a real failing module test.

The negative smoke classifiers now require the contract-specific empty arrays. `crates/jurisearch-deploy/src/ops/smoke.rs:369-404` passes missing-id only on an existing empty `documents` array or served `NoResults`; leaked documents, unrelated documents, wrong success shapes, non-array/null fields, other served errors, and transport errors fail. `crates/jurisearch-deploy/src/ops/smoke.rs:413-448` passes bad-query only on served `BadInput` or an existing empty `candidates` array; candidates, `{}`, wrong `documents` shape, non-array/null `candidates`, unrelated served errors, and transport errors fail. The tests at `crates/jurisearch-deploy/src/ops/smoke.rs:572-645` cover the r2 false-green shapes called out in the instructions.

The `demo up` hard-fail path now matches the documented contract. `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:501-510` runs bounded catch-up only for `demo`, calls the pure `demo_catchup_blocking_reason`, and returns `Err` before readiness and before `UNIT_SITE` start at `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:518-533` when catch-up is non-green. Non-demo `site install` still calls `run_install(&args, false)` at `crates/jurisearch-deploy/src/bin/jurisearchctl.rs:258`, so it does not enter the demo-only catch-up block.

The r1/r2 accepted watchdog fixes still hold in source: the watchdog parser accepts PostgreSQL's space-separated `timestamptz::text` with numeric offsets at `crates/jurisearch-deploy/src/ops/watchdog.rs:270-298`; behind cursors with unknown/unparseable age fail closed to `StalledCursor` at `crates/jurisearch-deploy/src/ops/watchdog.rs:131-144`; and the watchdog path reads verifier/manifest/cursor/status only, with no `run_catchup` or apply call, at `crates/jurisearch-deploy/src/ops/watchdog.rs:197-228`. The tests at `crates/jurisearch-deploy/src/ops/watchdog.rs:350-435` keep `no_new_packages`, stale behind, recent behind, unknown-age fail-closed, and real PG timestamp parsing distinct.

The guarded fixture deferral is honest. `crates/jurisearch-deploy/src/ops/fixture.rs:77-98` fails fast when the configured `sync.source_root/<corpus>/manifest.json` is absent, and `crates/jurisearch-deploy/fixtures/README.md` documents that the committed bytes are deferred and the live fixture demo is unavailable until generated and committed. The scope remains confined to `jurisearch-deploy` plus the runbook/docs, and `jurisearch-deploy` depending on `jurisearch-client` does not widen the thin-client dependency cone.

VERDICT: FIXES_REQUIRED
