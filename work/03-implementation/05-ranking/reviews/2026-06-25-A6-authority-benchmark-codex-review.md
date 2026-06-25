# Codex Review: A6 Authority Benchmark

## Findings

### WARN: The benchmark cannot report the zone recall regression guard described by the A6 plan.

`EvalFranceJurisAuthorityArgs` exposes judicial/admin qrel limits, `--authority-weights`, `--source-revision`, and `--out`, but no `--include-zones` switch, and `eval_france_juris_authority_payload` only scores `gold["judicial_retrieval"]` and `gold["administrative_retrieval"]`. The A6 plan says to report "zone recall@10 OFF vs ON if `--include-zones` is set", and design §7.1 includes zone recall as part of the authority regression guard. As implemented, operators cannot run that optional guard from this benchmark at all, so a zone-path authority regression would be invisible in the A6 artifact.

Concrete fix: add an `include_zones: bool` flag to `EvalFranceJurisAuthorityArgs`; when set, build the zone qrels with the existing zone gold path and score OFF/ON with the same production zone search path used by `eval france-juris-zones`, then include a measured-only `zones` block and fold it into the advisory recall guard.

### WARN: `authority_lift_delta` is only category-level, not broken down by source/pair as required.

`authority_category_json` reports `authority_lift_off`, per-weight `authority_lift_on`, and `authority_lift_delta` only for the whole judicial or administrative category. The only source-level field is `source_pairs`, which is a coverage count such as `cass>inca`, not an OFF/ON/delta metric. Design §7.2 asks for the ON-minus-OFF lift "per source/order" with coverage and score-gap reporting, and the A6 plan asks for a "per-order/per-source breakdown". The current shape can show that source-pair coverage exists, but it cannot reveal that one source pair regressed while another improved inside the same order.

Concrete fix: accumulate pair totals and OFF/ON above counts by source-pair key (or by `{order, higher_source, lower_source}`), then emit per-weight `authority_lift_off`, `authority_lift_on`, and `authority_lift_delta` for each source-pair alongside the existing category aggregate.

### NIT: The artifact reason says the recall guard "confirms" safety even when it fails.

`authority_benchmark_artifact` always emits a fixed `reason` saying "the recall regression guard confirms authority never buries the gold below OFF or the 0.50 floor", but the same artifact can contain `"recall_regression_guard": { "ok": false }` and the unit test explicitly covers that measured-but-failing state. This is not a gating bug because the structured boolean is present, but the prose diagnostic is misleading when the guard fails.

Concrete fix: make the `reason` string conditional on `recall_guard_ok`, or use neutral prose such as "the recall regression guard records whether authority buried the gold below OFF or the 0.50 floor."

## Verified

- Reviewed `/tmp/codex-review-a6.md`, the A6 implementation plan, and design §7.
- Inspected the new `eval france-juris-authority` args, dispatch, artifact builder, pair formation, recall guard, and Phase 2 gate isolation.
- Ran `cargo test -p jurisearch-cli`.

VERDICT: FIXES_REQUIRED
