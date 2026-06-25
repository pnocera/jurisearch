# A3 Config Validation Review

## Findings

### WARN: `help schema` does not advertise the new session field

`SearchRequest` now accepts `authority_weight` and threads it into `RetrievalOptions` (`crates/jurisearch-cli/src/request.rs:51-64`), and clap exposes `--authority-weight` on one-shot `search` (`crates/jurisearch-cli/src/args.rs:200-205`). But the machine-readable `SearchRequest` schema still lists only the pre-existing request properties through `zone` and omits `authority_weight` (`crates/jurisearch-core/src/schema/search.rs:11-24`). This makes capability discovery false for JSONL/session clients even though `session_search_payload` deserializes the field through the shared `SearchRequest`.

Concrete fix: add `authority_weight` to `crates/jurisearch-core/src/schema/search.rs` with `type: "number"`, `minimum: 0`, `maximum: 1`, and a description matching the CLI/session contract (`0.0` is off, positive is decision-only and first-page-only). Regenerate/update `crates/jurisearch-core/src/schema_golden.json`, and extend `crates/jurisearch-cli/tests/cli_help_contract.rs` to assert the schema advertises the field and its bounds.

### WARN: The A3 validation acceptance matrix is not covered by tests

The A3 plan explicitly requires CLI contract tests for unset, `0.0`, positive, negative, `>1.0`, non-finite if representable, `--kind code`, `--kind all`, cursor plus positive weight, and session parity (`work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:243-249`). The implementation adds the validations in `validate_retrieval_options`, `search_payload`, and `zone_search_payload` (`crates/jurisearch-cli/src/query_support.rs:43-50`, `crates/jurisearch-cli/src/retrieval/search.rs:47-66`, `crates/jurisearch-cli/src/retrieval/zone.rs:63-66`), but no test file currently mentions `authority_weight`, `authority-weight`, or these new rejection paths. The existing schema contract test still passes because it does not assert the new property.

Concrete fix: add focused CLI/session contract tests under `crates/jurisearch-cli/tests/cli_retrieval_contract.rs` or equivalent. They should prove `0.0` bypasses authority-specific rejections, positive weight rejects `kind=code`/`kind=all` and inbound cursor before index/query execution, invalid numeric values return `bad_input`, and JSONL `search` accepts/validates `authority_weight` through the same shared path.

## Verification

- `cargo check -p jurisearch-cli` passed.
- `cargo test -p jurisearch-cli help_schema_json_is_valid_and_lists_commands -- --exact` passed, but that test does not cover `authority_weight`.

VERDICT: FIXES_REQUIRED
