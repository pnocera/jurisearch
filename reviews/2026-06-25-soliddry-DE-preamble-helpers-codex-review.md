# Code Review: SOLID/DRY D+E

## Findings

### NIT: `stats_payload` still duplicates the storage JSON parse bridge

`crates/jurisearch-cli/src/status.rs:284` still inlines the exact `serde_json::from_str(&response).map_err(|error| dependency_unavailable(error.to_string()))?` bridge that `parse_storage_json` was added to centralize. Keeping `stats` out of `open_query_index` is correct because it deliberately has no query-readiness gate, but that does not require keeping the parse boilerplate local.

Concrete fix: change the parse in `stats_payload` to:

```rust
let stats: Value = parse_storage_json(&response)?;
```

## Verification Notes

- `open_query_index` preserves the prior preamble order: `require_existing_index_dir` first, then `open_index`, then `ensure_query_readiness`, with the caller-supplied gate. That preserves index-dir validation before Postgres startup errors before readiness errors.
- Gates are unchanged at the adopted sites: `fetch`, `cite`, `context`, `related`, `inspect`, `versions`, and `diff` still use `QueryReadinessGate::Fetch`; `compare` still uses `QueryReadinessGate::Search`.
- `cite_payload` still opens the index only inside `if let Some(lookup_target) = parsed.lookup()`, so malformed/no-lookup citations do not touch the index. `compare_payload` still performs empty-query, `top_k`, and searchable-token validation before opening the index.
- `parse_storage_json` is behavior-equivalent to the inlined bridge at the adopted sites, including final-expression use in `related_payload`, because it maps the serde error string directly through `dependency_unavailable`.
- Command-specific validation and result semantics remain local: `fetch` keeps `ids`/`part` validation and no-document handling; `context` keeps ID/as-of validation and target-null handling; `related` keeps depth/relation validation; `inspect`, `versions`, and `diff` keep their ID/date and no-results checks.
- The deliberate exclusions are correct: `search` keeps readiness inside `search_with_postgres` because it chooses `Search` vs `SearchLexical` from the effective mode and is reused by eval/batch callers; `zone` keeps `ensure_zone_retrieval_readiness`; `stats` keeps no readiness gate.
- `every_session_available_command_reaches_a_handler` maps the current command inventory correctly: `help schema --json` derives `help schema`, `help agent` stays `help agent`, `model fetch` stays `model fetch`, and `eval phase1` stays `eval phase1`. Skipping only `session --jsonl` and `batch --jsonl` matches the contract comments and `command_session_available`.
- The session-alignment test would fail for the intended drift cases: a non-excluded command with no dispatcher arm falls through to `unknown session command`, and a one-shot-only command left non-excluded also falls through instead of returning `not_implemented`.
- I did not rerun the full cargo suite; this review was source/diff-based against commit `675378c`.

VERDICT: GO
