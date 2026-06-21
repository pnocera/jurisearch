# Claude Review: Query Readiness Gates

Verdict: GO

Reviewed commit `b153c0b` "Gate retrieval on ingest health coverage" against
`work/03-implementation/IMPLEMENTATION_PLAN.md` Phase 1.0 acceptance, in
particular: "Query access is blocked or explicitly marked incomplete until
required projections pass their gates." (plan:488, plan:104). HEAD == `b153c0b`,
working tree clean except untracked `.codegraph/` (excluded from scope).

The change is correct, plan-aligned, and preserves JSON/exit discipline. One
low-severity plan-wording inaccuracy and several non-blocking test/UX
suggestions are noted. None block the push.

## Findings

- **Low / plan accuracy — `work/03-implementation/IMPLEMENTATION_PLAN.md:497`.**
  The new status line says *"`fetch` requires projection coverage **for requested
  stored documents**."* The implementation does **not** scope projection to the
  requested IDs — `ensure_query_readiness` (`main.rs:1407`) checks the
  **corpus-wide** projection metric from `load_ingest_health`
  (`ingest_accounting.rs:487`, `count(DISTINCT document_id)` over the whole
  `documents` table). A reader could infer per-ID gating that does not exist.
  Impact: documentation/contract drift only; behaviour is corpus-wide blocking.
  Required fix: reword to "`fetch` requires complete corpus projection coverage"
  (or implement per-ID scoping if that was the intent — see Suggestions).

## Suggestions

- **Test gap on the projection branch and the fetch blocking path.** The new
  test (`cli_contract.rs:311`) only exercises the *embedding* sub-gate for
  `search`. The `"projection coverage gate is incomplete"` branch
  (`main.rs:1416`) is never executed, and `fetch`'s blocking path is never
  exercised (the existing `fetch_returns_documents_from_existing_index` test
  only hits the *pass* path + `no_results`). Risk is low because both branches
  share the identical `coverage_complete` + `index_not_query_ready` code path
  that the embedding test already covers end-to-end, and the search/fetch
  asymmetry is implicitly proven (the fetch test succeeds with projection 1/1
  but embedding 0/1). Still, the gate the plan most emphasizes ("required
  *projections*") has no direct assertion. Recommend a test that inserts a
  document with **no** chunk (projection incomplete) and asserts both `search`
  and `fetch` return `index_unavailable` + `"projection coverage gate is
  incomplete"` + exit 3.

- **Empty-index behaviour is untested and silently changed.** With `total == 0`,
  `coverage_complete` returns `false` (`main.rs:1388`), so an empty-but-
  initialized index now returns `index_unavailable` (exit 3) for both commands,
  where previously `search`/`fetch` would have reached `no_results` (exit 2).
  This is arguably more correct ("not query-ready" vs "no results"), but it is
  an agent-contract change with no test. Worth an explicit test/assertion so the
  exit-code semantics are pinned.

- **Corpus-wide gate is coarse.** A single un-chunked document, or a
  `ingest embed-chunks --limit` smoke run, leaves the *entire* retrieval surface
  blocked until coverage reaches 100%. This matches the plan's permitted
  "blocked" branch, but for `fetch` (which names specific IDs) it is heavy-
  handed — a fully-projected document cannot be fetched if some unrelated
  document is incomplete. Consider, in a later slice, scoping `fetch`'s gate to
  the requested IDs (which would also make plan:497's wording literally true).

- **Embedding coverage ignores fingerprint.** `embedding_coverage`
  (`ingest_accounting.rs:499`) counts any `chunk_embeddings` row regardless of
  `embedding_fingerprint`, while `hybrid_candidates_json` filters dense
  candidates by the configured fingerprint. A fully-but-stale-embedded corpus
  would pass the gate yet yield zero dense candidates (silent BM25-only). This
  is a pre-existing storage-metric limitation, not introduced here, but the gate
  now makes a query-readiness *decision* on it. Relevant to the Phase 1.7
  re-embed/migration gate; worth a forward note rather than a fix now.

## Verification Notes

Inspected:
- Gate wiring: `search_payload` calls the gate before any embedding-endpoint
  work (`main.rs:402`, ahead of `OpenAiCompatibleClient::new`/`embed_query` at
  405–409); `fetch_payload` gates at `main.rs:457`. Endpoint avoidance on an
  incomplete index is real, not just claimed.
- `ensure_query_readiness` (`main.rs:1407`): projection required for both gates;
  embedding required only for `Search` (`matches!(gate, ..::Search)`), so `fetch`
  intentionally does not require embeddings. Session paths route through the same
  `search_payload`/`fetch_payload`, so the gate covers JSONL mode too.
- Exit/JSON discipline: gate errors use `ErrorCode::IndexUnavailable` →
  `ProcessExit::Local` = 3 (`jurisearch-core/src/error.rs:68`), emitted as
  JSON-only on stdout via `emit_error`; stderr stays empty (asserted by the
  test). Error message reports the failing command, the reason, and both
  coverage ratios; suggestions point to `status` and the ingest commands.
- `status --json` consistency: `status_index_and_ingest_health`
  (`main.rs:1306`) derives `query_ready` from the *same* `coverage_complete`
  checks (projection AND embedding), so the gate and the status flag cannot
  disagree.
- Other implemented retrieval surface: `cite`, `related`, `context`, `expand`
  are all `not_implemented` (`main.rs:338–369`); `search`/`fetch` are the only
  implemented retrieval commands, and both are gated — no ungated query path.
- Plan delta: the "block/mark query access … outside the status report" item was
  correctly moved from "Remaining" to "Done" (`IMPLEMENTATION_PLAN.md:497–498`).

Commands run (managed Postgres available via `~/.pgrx` 18.4; forced execution
with `JURISEARCH_REQUIRE_PG_EXTENSIONS=1` so tests could not silently skip):
- `cargo test -p jurisearch-cli status_marks_initialized_index_not_ready_when_embedding_coverage_is_incomplete --test cli_contract` → **1 passed** (ran against real PG, 1.72s; gate exercised, not skipped).
- `cargo test -p jurisearch-cli --test cli_contract fetch_returns_documents_from_existing_index` → **1 passed** (regression: projection-complete fetch + `no_results` for missing ID still exit 2; fetch succeeds with 0 embeddings — asymmetry confirmed).
- `cargo test -p jurisearch-cli --test cli_contract status_reports_ingest_health_from_existing_index` → **1 passed** (full coverage → `query_ready: true`).
- `git diff --check` → clean.
- Confirmed HEAD == `b153c0b`; working tree clean apart from untracked `.codegraph/`.

Relied on the pre-run `cargo test --workspace` and `cargo clippy --workspace
--all-targets -D warnings`; the crate compiled cleanly during my targeted test
runs, consistent with that.
