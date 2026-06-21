# Review: Phase 1.3 retrieval ablation modes (hybrid / bm25 / dense)

Scope reviewed (uncommitted working tree): storage `RetrievalMode` support in `hybrid_candidates_json`, CLI `search --mode` + session JSON `mode`, `help schema --json` enum/response updates, retrieval smoke + CLI contract tests, and the implementation-plan status update.

## What I verified

- **Builds and lints clean.** `cargo build -p jurisearch-cli`, `cargo test --workspace --no-run`, `cargo fmt --check`, and `cargo clippy -p jurisearch-storage -p jurisearch-cli -p jurisearch-core --tests` all pass with no warnings.
- **Mode parsing is real, not assumed.** Ran the binary: `--mode` advertises `[default: hybrid] [possible values: hybrid, bm25, dense]`; `--mode bm25`/`dense` parse; `--mode sparse` is rejected by clap with the correct possible-values list. clap's `ValueEnum` renders `Bm25` → `bm25` (the digit does *not* become `bm-25`), so it matches the schema, the contract test, and the `serde(rename_all="snake_case")` session path.
- **DB-free contract test passes** (`help_schema_json_is_valid_and_lists_commands`), covering the new `common_enums.search_mode` and `SearchRequest.properties.mode.default == "hybrid"` assertions. The bm25/dense storage + CLI assertions need Postgres and were reviewed by reading, not executed.
- **All 7 `HybridCandidateQuery` construction sites updated** (1 in CLI, 6 across the 3 storage test files) — no straggler breaks from the struct's new `Option` fields + `retrieval_mode`.
- **Hybrid is a behavior-preserving extraction.** The hybrid arm of `ranked_candidate_ctes` reproduces the original `lexical → dense_pool → dense → fused → ranked` chain verbatim, including the RRF formula; the outer `limited`/JSON projection and `dense_pool_limit` are unchanged logic, just factored out. The unchanged hybrid smoke assertions (chunk_id, `lexical_rank=1`, cursor suffix) corroborate this.

## Findings

1. **Strong: BM25 mode genuinely decouples from embeddings.** `uses_dense()` gates both embedding generation *and* the readiness gate (`SearchLexical` checks projection coverage only, skips the embedding-coverage gate). The new `cli_contract` case proves it end-to-end: it points `JURISEARCH_EMBED_BASE_URL` at an unreachable port (`127.0.0.1:9`) and `--mode bm25` still succeeds with `retrieval_mode: "bm25"` and `dense_rank: null`, while the same index rejects default (hybrid) search with `index_unavailable`. This is exactly the intended value of the slice and is the right test design.

2. **Strong: the `Option` API removes the fake-vector anti-pattern.** Making `query_embedding`/`embedding_fingerprint` optional and routing BM25 through a no-embedding CTE means BM25 queries no longer thread a placeholder vector through dense SQL. `dense_query_inputs` is a clean precondition guard, and the new `StorageError::Retrieval` variant degrades gracefully (maps to `dependency_unavailable`, no panic) if a caller ever sets a dense mode without a vector.

3. **Strong: per-mode RRF is consistent.** BM25-only `fused_score = 1/(60+lexical_rank)` and dense-only `1/(60+dense_rank)` are exactly each arm's contribution in the hybrid sum, so scores/cursors stay comparable in shape across modes. The smoke tests pin the discriminating invariants (`dense_rank: null` for bm25, `lexical_rank: null` for dense).

4. **Strong: schema/response/session kept in sync.** `common_enums.search_mode`, `SearchRequest.mode` (default `hybrid`), and the new `SearchResponse.retrieval_mode` enum all match the CLI default and the SQL-emitted `retrieval_mode` field; the session `SessionSearchArgs.mode` defaults via `default_search_mode()` and is forwarded into `SearchArgs`.

5. **Low — dense mode on the direct CLI path loses the empty/non-tokenizable query guard.** In `search_payload`, the `must contain at least one searchable token` check only runs when `uses_lexical()`; for dense it falls back to `args.query.trim()`. The direct `emit_search` path has no other empty-query check, so `jurisearch search "" --mode dense` (or a punctuation-only query) skips the clean `bad_input` and instead calls `embed_query("")`, surfacing an upstream/dependency error. The session path guards only fully-empty/whitespace, not punctuation-only. Functional impact is a worse error message on an edge case, not wrong results.

6. **Low — no CLI-level coverage for `--mode dense`.** Dense is exercised at the storage layer (`retrieval_smoke` dense block) and the embedding call is exercised via the hybrid live-embeddings test, but no CLI test drives `--mode dense` through the `Search` readiness gate + embedding call + `retrieval_mode: "dense"` echo. bm25 and hybrid both have CLI coverage; dense is the gap.

7. **Info — echoed `query` field differs by mode.** For hybrid/bm25 the response `query` is the parade-normalized token string; for dense it is the raw trimmed input (query_text is only echoed in dense mode, never used in the SQL). Harmless and arguably more faithful for dense, but slightly inconsistent across modes.

8. **Info — `DESIGN.md` search-flag table omits `--mode`.** `help schema --json` is the documented arbiter and it is updated, so this is not a contract gap; the design doc's command table (`work/01-design/DESIGN.md:310`) could optionally gain `--mode` for completeness.

## Recommendations

- (Optional, low effort) Extend the dense-mode `query_text` branch to reject an empty/non-tokenizable trimmed query with the same `bad_input` as the lexical path, restoring validation parity before the embedding endpoint is called (Finding 5).
- (Optional) Add a `--mode dense` CLI assertion — piggybacking on the existing live-embeddings test is cheapest — to cover the dense readiness-gate + embedding + echo path (Finding 6).
- (Nice-to-have) Add `--mode` to the `DESIGN.md` search command row (Finding 8).

None of the above blocks merge. The change is correct, cleanly factored (hybrid preserved as a pure extraction), well-tested at the storage layer with a sharp CLI contract test for the BM25-without-embeddings case, and the plan status update accurately reflects the delivered scope.

Verdict: GO
