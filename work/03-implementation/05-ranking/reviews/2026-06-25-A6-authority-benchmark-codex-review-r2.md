# Codex Review: A6 Authority Benchmark R2

## Findings

No BLOCKER, WARN, or NIT findings.

The current change addresses the prior review items:

- `eval france-juris-authority` now exposes `--include-zones` and folds optional zone OFF/ON recall into the same advisory regression guard.
- Pairwise authority lift is reported both as an aggregate and under `lift_by_source_pair`, with OFF lift, ON lift, delta, and pair coverage for each source-pair key.
- The artifact `reason` is conditional on `recall_regression_guard.ok`, so a failed guard no longer reads as a safety confirmation.

## Verified

- Read `/tmp/codex-review-a6-r2.md` and checked the current implementation against design section 7 and the A6 implementation plan.
- Inspected `EvalFranceJurisAuthorityArgs`, `eval_france_juris_authority_payload`, pair formation, per-source-pair lift aggregation, optional zone recall, artifact assembly, and Phase 2 gate isolation.
- Confirmed the recall guard uses the same France-juris top-10 scoring pattern: a 40-document production search page, scored over the first 10 document ids.
- Ran `cargo test -p jurisearch-cli` successfully.

VERDICT: GO
