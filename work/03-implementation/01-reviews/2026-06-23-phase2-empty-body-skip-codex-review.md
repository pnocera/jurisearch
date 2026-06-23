# Code Review: Phase 2 Empty-Body Decision Skip

Reviewed change at `HEAD` (`e58618e`, `git diff HEAD~1 HEAD`).

## BLOCKER

None.

## WARN

None.

## NIT

1. `crates/jurisearch-cli/tests/cli_contract.rs:4668` only pins the top-level `skipped_empty_body_members` field, while the implementation also adds the same counter to `manifest.coverage` in `crates/jurisearch-cli/src/main.rs:4144`. Because `status` and coverage reporting consume latest-run manifests, a future regression could drop the manifest field while preserving the direct command output. Concrete fix: add `assert_eq!(json["manifest"]["coverage"]["skipped_empty_body_members"], 1);` next to the existing top-level assertion.

## Validation

- `cargo test -p jurisearch-ingest empty_body_decision_is_typed_not_built` passed.
- `cargo test -p jurisearch-cli --test cli_contract ingest_juri_archives_skips_empty_body_decisions -- --nocapture` passed.

VERDICT: GO
