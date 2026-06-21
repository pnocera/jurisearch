# Search Cursor Pagination — Code Review (Phase 1.3)

**Date:** 2026-06-21
**Reviewer:** Claude (Opus 4.8)
**Scope:** Uncommitted diff implementing keyset cursor pagination for `search` —
storage `after_cursor` filtering over ranked candidates, CLI/session cursor input
parsing, `top_k+1` overfetch + `next_cursor` emission, schema/tests/plan updates.

Files reviewed: `jurisearch-storage/src/retrieval.rs`, `jurisearch-cli/src/main.rs`,
`jurisearch-core/src/schema.rs`, `cli_contract.rs`, `retrieval_smoke.rs`,
`legi_canonical_retrieval.rs`, `target_spike_corpus.rs`, `IMPLEMENTATION_PLAN.md`.

Build status: `cargo check --workspace --tests` clean; `cargo clippy --workspace --tests` clean.

---

## Summary

A correct, well-scoped keyset (seek) pagination implementation. The cursor is a
`<rounded_score>:<chunk_id>` pair; the storage layer adds a single `WHERE` predicate
to the `limited` CTE that resumes strictly after the cursor in the existing rank order,
and the CLI over-fetches one row (`top_k+1`) to decide whether a `next_cursor` exists.
Ranking CTEs are untouched, so ranking order is preserved and only the window shifts —
the core requirement is met. Input is validated and SQL-escaped; tests cover the storage
keyset, an end-to-end CLI second page, and bad-cursor input. One real (low-probability)
keyset/ordering inconsistency and one design limitation are noted below.

---

## Findings

### F1 — (Medium, low-probability) Keyset key ≠ ORDER BY key: raw vs. rounded score
`retrieval.rs:106` and `retrieval.rs:137` order by the **raw** `fused_score`
(`ORDER BY r.fused_score DESC, r.chunk_id`), but the cursor value (`retrieval.rs:135`)
and the keyset predicate (`retrieval.rs:153-165`) both key on the **rounded** score
(`round(r.fused_score::numeric, 8)`). For sound keyset pagination the sort key and the
seek key must be identical. They are not.

Failure mode: two adjacent candidates A, B whose raw scores differ only beyond the 8th
decimal so they round to the same bucket, with `A.raw > B.raw` (A precedes B in the true
order) but `A.chunk_id > B.chunk_id`. If A is the page boundary (the emitted cursor),
page 2's predicate `round=cursor AND chunk_id > A.chunk_id` excludes B even though B
truly ranks *after* A → **B is silently skipped** (the symmetric arrangement yields a
duplicate).

Probability is mode-dependent:
- **bm25 / dense modes:** effectively zero. `fused_score = 1/(60+rank)` with unique
  integer ranks, so distinct scores are separated by ≫1e-8 and rounding never collides
  two distinct candidates. The tested path is safe.
- **hybrid mode:** plausible but rare. Fused RRF sums `1/(60+L)+1/(60+D)` over pools of
  ~`top_k×4` candidates can land within 1e-8 of each other; the bug then requires that
  near-tie to also straddle a page boundary with adverse `chunk_id` ordering — a
  conjunction of unlikely events, but not impossible.

This is a latent edge-case defect, not a common-case one, and the fix is trivial
(see Recommendations). Worth fixing before hybrid pagination is relied on.

### F2 — (Low/Medium, likely accepted scope) Pagination depth is bounded by the relevance pool
The ranked pool is the top `lexical_limit`/`dense_limit` (`= top_k × 4`) candidates; the
cursor filters *within* that fixed pool, recomputed identically on each request. So at a
constant `top_k`, a client can walk at most ~`top_k×4` candidates total (≈4 pages) before
`next_cursor` disappears — even when the corpus holds many more matches. Because the pool
shrinks/grows with the *request's* `top_k`, this is internally consistent across pages,
but the client cannot distinguish "relevance pool exhausted" from "corpus exhausted":
end-of-pages is reported the same way in both cases (silent boundary). This matches
search's existing top-N-by-relevance semantics (it never scanned the whole corpus), so
it is probably acceptable for Phase 1.3 — but it should be documented so consumers don't
treat exhausted pages as an exhaustive result set.

### F3 — (Nit) `next_cursor` silently depends on the per-candidate `cursor` field always being present
`main.rs:548-557` truncates to `top_k` then reads `candidate["cursor"]`; if that field
were ever absent it would truncate the page yet emit no `next_cursor`, stranding the
client mid-stream. It is currently safe — the SQL always emits `cursor` as a non-null
`concat(round(...)::text, ':', chunk_id)` over non-null columns — so this is only a
latent coupling worth a comment, not a bug.

### F4 — (Nit) Cursor replay relies on the client resending query/mode/as_of
`pagination` echoes `after_cursor` but not the query, `mode`, `kind`, or `as_of`. With a
defaulted (unpinned) `as_of`, paginating across a date boundary shifts the validity
window and thus the ranking, breaking page continuity. The `cursor_note` does say "with
the same query/filter inputs," so this is adequately disclosed; flagging only so the
contract expectation is explicit.

---

## Things verified correct

- **Keyset predicate logic** (`retrieval.rs:153-165`): `score < cursor OR (score = cursor
  AND chunk_id > cursor_chunk)` correctly resumes after the cursor under
  `fused_score DESC, chunk_id ASC`. Exact ties (equal raw scores, common in hybrid where
  e.g. a lexical-only and a dense-only hit share `1/(60+rank)`) are handled by the
  `chunk_id` tiebreak and are immune to F1 since rounding preserves equality.
- **Overfetch / `has_more`** (`main.rs:519`, `549-565`): `query_limit = top_k+1`;
  `next_cursor` is emitted iff a `(top_k+1)`-th row exists, so the next page is guaranteed
  non-empty — no off-by-one and no spurious trailing empty page. `returned` is computed
  after truncation; top-level `limit` is restored to `top_k`.
- **`possibly_truncated` semantics improved**: now `has_more` (a real next page) rather
  than the old "window full" heuristic — strictly more accurate (e.g. exactly `top_k`
  results now correctly reports `false`). This is a contract change but a positive one.
- **Input validation & injection safety** (`main.rs:2057-2080`): `split_once(':')`,
  finite/non-negative `f64` parse, non-empty `chunk_id`; the **original** score string
  (not the lossy `f64`) is forwarded, preserving precision. Both fields pass through
  `sql_string_literal`, and the score is bound as `'…'::numeric` — no injection surface.
- **Round-trip consistency**: cursor and predicate both round to 8 decimals via exact
  `numeric`, so `round(score) = '…'::numeric` holds exactly (no float drift).
- **Struct fan-out**: the new `after_cursor` field is supplied at all four
  `HybridCandidateQuery` construction sites (CLI + 3 test files); session path threads
  `cursor` through (`main.rs:700`).
- **Schema** (`schema.rs`): `SearchRequest.cursor`, `pagination.after_cursor`,
  `retrieval.query_limit`, `retrieval.after_cursor` all match emitted fields.
- **Tests**: `retrieval_smoke` asserts page-2 `chunk_id` differs from page-1;
  `cli_contract` drives a real second page via the returned `next_cursor` and asserts a
  distinct document + echoed `after_cursor`, plus a `code(2)` bad-cursor case. Meaningful
  coverage. (Note: the storage keyset test exercises bm25 only — the mode immune to F1;
  a hybrid near-tie boundary is not exercised.)

---

## Recommendations

1. **(F1, fast-follow) Unify the sort key and the keyset key.** Order by the same rounded
   expression used in the cursor — change `ORDER BY r.fused_score DESC, r.chunk_id`
   (`retrieval.rs:106`) and the final `jsonb_agg` `ORDER BY fused_score DESC, chunk_id`
   (`retrieval.rs:137`) to `ORDER BY round(fused_score::numeric, 8) DESC, chunk_id`. This
   makes the displayed score, the emitted cursor, the sort order, and the seek predicate
   all share one canonical key, eliminating the skip/duplicate window. (It can reorder two
   sub-1e-8-apart hybrid candidates vs. the raw order, but that difference is below the
   reported precision and is the price of a consistent keyset.)
2. **(F2) Document the pagination depth bound** in the cursor note / plan: pages walk the
   relevance pool (~`top_k×4`), not the full corpus, and exhausted pages do not imply an
   exhaustive result set. If deeper paging is later required, decouple the pool size from
   the per-request `top_k`.
3. **(F3, optional) Add a one-line comment** at `main.rs:552` noting `next_cursor` relies
   on the SQL always projecting a `cursor` field, so the invariant isn't lost in refactors.
4. **(Tests, optional) Add a hybrid-mode keyset boundary test** to guard the F1 fix and
   the tie-break behavior that bm25-only coverage cannot exercise.

None of the above blocks the increment: the common-case behavior is correct, validated,
escaped, and well-tested; F1 is a rare hybrid-only edge case with a trivial fix and F2 is
an accepted scope boundary. Recommend landing with F1 tracked as a fast-follow.

Verdict: GO
