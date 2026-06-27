# P7 Planner + Size-Driven Catch-Up Review

## Findings

### BLOCKER: Installed-client baseline fallbacks can return an inapplicable `FreshBaseline`

The required P7 adjustment says a baseline fallback for an already-installed client must be forward-moving, and an installed long-offline client needs a re-baseline-style supersession rather than a first-baseline package. The implementation only enforces the forward-moving check in the `min_available_sequence` branch:

- `crates/jurisearch-syncd/src/planner.rs:125` checks `active_baseline.sequence > cursor.sequence` only when `cursor.sequence < min_available_sequence`.
- The other installed-client fallback paths call `baseline_or_blocked(&manifest.active_baseline)` without the cursor: missing/duplicate/non-`+1` chain at `planner.rs:136`, bounded `RequiresBaseline` at `planner.rs:141`, and the §9.4 size/reissue policy at `planner.rs:178`.
- `baseline_or_blocked` at `planner.rs:187` checks only baseline minimum client version and schema version, then returns `FreshBaseline`; it does not know whether the baseline advances the cursor or whether it is `PackageKind::Rebaseline` for an installed corpus.

That is not just a prefilter quality issue. The applier proves these routes are not catch-up-capable: `apply_baseline` rejects an installed corpus that is behind the media package (`apply.rs:668`), and media with `result_sequence` lower than the cursor is rejected as `WrongGeneration` (`apply.rs:660`). A same-sequence media package is only an idempotent no-op if the package id/digest already match (`apply.rs:644`), so it also cannot catch the client up to the remote head.

The unit tests currently encode the false green. `chain_manifest()` uses `baseline(1)` with `PackageKind::Baseline`, while the relevant cursor is at sequence `2`; nevertheless `a_gap_in_the_chain_routes_to_baseline`, `a_fingerprint_reissue_in_the_chain_routes_to_baseline`, `a_requires_baseline_entry_routes_to_baseline`, the policy flip test, and the bounded-range test all accept `FreshBaseline` for that non-forward first-baseline fixture (`planner.rs:543`, `planner.rs:608`, `planner.rs:617`, `planner.rs:628`, `planner.rs:669`, `planner.rs:684`).

Concrete fix: split the helper into fresh-client and installed-client paths, or pass the cursor sequence/mode into it. For every installed-client fallback, require `active_baseline.sequence > cursor.sequence`; if not, return `Blocked { code: BaselineRequired, ... }`. Also require an installed-client fallback artifact to be a re-baseline/supersession (`BaselineRef.package_kind == PackageKind::Rebaseline`) before planning it as catch-up, leaving true first baselines for `cursor == None`. Then update the positive fallback tests to use a forward `Rebaseline` fixture, and add negative tests for non-forward and first-baseline installed fallbacks.

## Notes

The requested contract fields are present in `BaselineRef` and `CatchupPolicy`, `ClientCursor` reads the full cursor stamps, the ratio checks use integer `u128` products with a zero-baseline guard, and `run_catchup` binds each fetched artifact's embedded `artifact_sha256` to the signed remote `sha256` before applying. The loop also rejects `Blocked` plans before fetching and applies incrementals in order through `apply_incremental`, so the remaining issue is the planner's invalid baseline route for installed clients.

VERDICT: FIXES_REQUIRED
