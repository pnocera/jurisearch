# Code Review: SOLID/DRY #20 Shared Command Request Structs

## Findings

No findings.

## Review Notes

- Verified the new `SearchRequest`, `RelatedRequest`, `CompareRequest`, and `EvalPhase1Request` serde defaults against the deleted session DTOs and the existing clap defaults. The highlighted defaults for search kind/mode/format/group_by/top_k, related rel/limit/depth, compare kind/top_k, eval phase1 mode/top_k, and `index_dir: Option<PathBuf>` are preserved.
- Checked the moved boundary validation in the payload builders. The one-shot and session paths now share the same validation entry points; search preserves empty-query before top_k before retrieval-options before zone routing, compare preserves empty-query before top_k, and related now keeps empty id ahead of depth/rel checks with the one-shot message as the canonical message.
- Checked `index_dir` flow through one-shot dispatch, session deserialization, `eval phase1`, France-LEGI, France-juris, and generic eval call sites. The global one-shot `--index-dir` is moved into the request only in mutually exclusive match arms, session `index_dir` remains request-owned, and eval callers either pass the already-open Postgres to `search_with_postgres` with `index_dir: None` or rebuild a `SearchRequest` with the mapped path where `search_payload` still opens the index.
- Reviewed the `SearchArgs` to `SearchRequest` conversion inside `search_with_postgres`; field access for query, as_of, group_by, top_k, cursor, retrieval options, and decision filters remains equivalent.
- Confirmed the schema remains hand-maintained in `jurisearch-core`; the compiled schema golden test passes and no schema file changes were required.

## Verification

- `cargo test -p jurisearch-core compiled_schema_matches_golden`
- `cargo test -p jurisearch-cli`

Note: `cargo test -p jurisearch-cli` passed with the existing live embedding endpoint tests left ignored by the test suite.

VERDICT: GO
