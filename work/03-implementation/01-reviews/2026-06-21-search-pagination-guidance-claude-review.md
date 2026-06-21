# Claude Review — Search Pagination/Truncation Guidance (Phase 1.3)

Reviewed: uncommitted Phase 1.3 change adding pagination/truncation metadata to `search`
responses (`jurisearch-cli/src/main.rs`, `jurisearch-cli/tests/cli_contract.rs`,
`jurisearch-core/src/schema.rs`, `work/03-implementation/IMPLEMENTATION_PLAN.md`).
Reviewer: Claude (Opus 4.8), 2026-06-21.

Scope claim — "adds pagination metadata without changing retrieval" — holds. The new block is
attached to the already-built response *after* `hybrid_candidates_json` returns; no query
parameter, limit, ordering, or candidate field is altered. Compiles clean
(`cargo check -p jurisearch-cli -p jurisearch-core`); the no-Postgres schema contract test passes.

## Findings

- **Correct, retrieval-neutral, additive.** `search_payload` (main.rs:506-519) derives
  `returned = candidates.len()` and sets a `pagination` object on the existing response value. Nothing
  upstream of the response (`HybridCandidateQuery`, `limit: args.top_k`, fusion, validity filter) is
  touched. The block is set *before* the empty-candidates check, so on the no-results error path it is
  correctly discarded (error payload, not a partial pagination block).
- **Truncation heuristic is sound and honestly labeled.** Retrieval caps candidates at
  `limit: args.top_k` (main.rs:497), so `returned <= top_k` always, making
  `possibly_truncated = returned >= top_k` (main.rs:507) equivalent to `returned == top_k` — i.e. "the
  window is exactly full." It over-reports when the corpus holds *exactly* `top_k` matches, which the
  `possibly_` prefix and the "Increase `--top-k`… cursor pagination is not implemented yet" wording
  correctly hedge. Acceptable for guidance metadata.
- **`top_k == 0` cannot reach the heuristic.** Both entry points guard it: CLI dispatch
  (main.rs:367-368) and `session_search_payload` (main.rs:616-617). So `returned >= top_k` is always
  evaluated with `top_k >= 1`; there is no `0 >= 0` true-on-empty footgun.
- **`cursor_supported: false` / "not implemented yet" is accurate.** No `cursor`/`after`/`offset`
  input argument exists on `SearchArgs` or `SessionSearchArgs`; nothing consumes a cursor. The claim
  is truthful.
- **Schema change is additive and consistent.** `schema.rs:114-124` adds `pagination` as an optional
  (non-required) object on `SearchResponse` with field-accurate types, including
  `next_cursor`/`guidance` as `["string","null"]` — matching the runtime, where `json!` serializes
  `Some(&str)`→string and `None`→null. No `additionalProperties:false` is in play, so the prior
  `expanded_terms`/`expansion_seed_version` and this field coexist without breaking validation. The
  schema test asserts `pagination.type == "object"`.
- **Test exercises the truncated branch.** The contract test forces `--top-k 1` against a single-doc
  fixture so `returned == top_k == 1`, then asserts all six pagination fields incl. `guidance`
  contains "Increase --top-k". Assertions match the code. No full-response equality assertions exist
  elsewhere, so the new field breaks no other test.
- **Plan update is accurate.** Adds a Done bullet describing the metadata and rewrites the Remaining
  line from "pagination/truncation guidance" to "cursor pagination" — exactly consistent with
  `cursor_supported: false`.

### Non-blocking observations

1. **Latent tension: candidates already carry a `cursor`, but `next_cursor` is null.** Each candidate
   emits `'cursor', concat(round(fused_score,8)::text, ':', chunk_id)` (`retrieval.rs:126`; also in the
   schema at `schema.rs:140`). The response therefore *already exposes* per-row cursor tokens while the
   new top-level block declares `cursor_supported: false, next_cursor: null`. This is internally honest
   (no endpoint consumes cursors yet), but an agent reading the response sees cursor tokens it is told
   are unusable — mildly confusing. Either populate `next_cursor` from the last candidate's `cursor`
   for forward-compat, or add a one-line note that per-candidate cursors are reserved scaffolding.
2. **Guidance text names a CLI flag the JSONL API doesn't expose.** The string hard-codes
   `--top-k`, but `session`/`batch` consumers set the `top_k` JSON field, not a CLI flag. For an agent
   driving the JSONL interface the advice references a knob that doesn't exist there. Consider wording
   that names the field (`top_k`) rather than the CLI spelling.
3. **Only the truncated branch is covered.** No test asserts the `returned < top_k` →
   `possibly_truncated:false`, `guidance:null` path. Cheap to add (e.g. the same fixture with
   `--top-k 10` asserting `possibly_truncated == false` and `guidance` null).
4. **Minor: redundant re-read of `candidates`.** `returned` is already computed at main.rs:506; the
   empty-results check at main.rs:520-522 re-reads `response["candidates"]`. `returned == 0` would
   reuse the value. Cosmetic.

## Recommendations

- Ship as-is; none of the above blocks. Before cursor pagination lands, resolve observation (1) so the
  per-candidate `cursor` and the top-level `next_cursor`/`cursor_supported` semantics agree.
- Fold (2) into the guidance wording so JSONL consumers get actionable advice, and add the
  non-truncated-branch assertion from (3) when next touching the contract test.

Verdict: GO
