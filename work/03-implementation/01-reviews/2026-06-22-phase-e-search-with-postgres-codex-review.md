Findings

- No BLOCKER findings.
- No WARN findings.
- No NIT findings.

Review notes

- Behavior preservation: the extracted tail in `crates/jurisearch-cli/src/main.rs:807` through `crates/jurisearch-cli/src/main.rs:920` matches the pre-extraction body in effect. The changed expressions are borrow/ownership adjustments only: `ensure_query_readiness(postgres, ...)` passes the same `&ManagedPostgres`; `query_text` passes the same `&str`; `after_cursor.map(...)` maps the already-borrowed `Option<&ParsedSearchCursor>`; `args.as_of.clone().unwrap_or_else(today_utc)` preserves the same `Option<String>` value; `args.query.clone()` writes the same diagnostics string; `Some(query_text)` serializes the same JSON string as `Some(query_text.as_str())`.
- Error precedence: `search_payload` still normalizes and rejects unsearchable queries before any index check at `crates/jurisearch-cli/src/main.rs:760` through `crates/jurisearch-cli/src/main.rs:772`, so a missing index cannot mask `search "!!!"` as `index_unavailable`. The unsupported `--kind decision` check still runs after `require_existing_index_dir` at `crates/jurisearch-cli/src/main.rs:772` through `crates/jurisearch-cli/src/main.rs:778`, preserving the previous precedence against missing-index errors.
- Search body ordering: the readiness gate, dense embedding preparation, `as_of`, kind filter, lexical/dense/query limits, cursor projection, `hybrid_candidates_json`, JSON parse, expansion metadata, pagination truncation, detailed diagnostics, and `no_results` conversion remain in the same order at `crates/jurisearch-cli/src/main.rs:807` through `crates/jurisearch-cli/src/main.rs:920`. I did not find dropped work, double computation, or observable drift from the added clones.
- Single-Postgres lifecycle: `search_with_postgres` is self-contained around an already-open `&ManagedPostgres`. It does not call `require_existing_index_dir` or `open_index`; those remain in `search_payload` at `crates/jurisearch-cli/src/main.rs:772` and `crates/jurisearch-cli/src/main.rs:780`. Its storage interaction is through readiness/candidate calls against the provided handle, so it is safe for a future loop that validates query/kind/cursor inputs and reuses one open index.
- Routing: CLI search, session search, and eval phase1 still route through `search_payload` (`emit_search`, `session_search_payload`, and `eval_phase1_fixture_result` are the only callers found). `search_with_postgres` currently has only the `search_payload` caller, so the extraction does not bypass existing CLI/session/eval validation paths.
- Verification performed for this review: inspected the live `git diff` for `crates/jurisearch-cli/src/main.rs`, compared against `HEAD:crates/jurisearch-cli/src/main.rs`, checked CodeGraph callers/callees for `search_payload` and `search_with_postgres`, and ran `git diff --check -- crates/jurisearch-cli/src/main.rs`. I did not rerun `cargo test -p jurisearch-cli`; the provided local result was 13 unit + 45 integration passed.

VERDICT: GO
