# P7 Planner + Size-Driven Catch-Up Review Round 2

## Findings

No findings.

The round-1 blocker is addressed in the actual source. `plan_catchup` now sends the fresh-client path through `baseline_compat_or_blocked`, while every installed-client baseline fallback path routes through `installed_baseline_or_blocked`: past retention, gap/duplicate/non-`+1` chain reconstruction failure, bounded `RequiresBaseline`, schema-ahead fallback, and the size/reissue baseline-preference branch.

`installed_baseline_or_blocked` only returns `FreshBaseline` when the active media root is `PackageKind::Rebaseline` and its sequence is strictly greater than the installed cursor sequence, then still applies the min-client-version and schema compatibility checks. A first `baseline` root and a non-forward rebaseline now both produce `Blocked { code: BaselineRequired, .. }`, matching the applier's first-baseline and rebaseline-forward guards.

The updated planner tests exercise the installed-client fallback route with a forced gap, and the two new negative tests cover both non-catch-up-capable cases from the blocker: first-baseline active root and rebaseline at/behind the cursor. The schema-ahead branch keeps returning `SchemaAhead` unless the installed-client helper actually yields a compatible forward rebaseline.

## Verification

- `cargo fmt --check`
- `cargo test -p jurisearch-syncd --lib planner -- --nocapture`
- `cargo test -p jurisearch-package-build --test catchup_loop -- --nocapture`

VERDICT: GO
