# Claude Review - Contextualized BM25

Verdict: GO

Reviewed: uncommitted Phase 1.2 BM25 contextualized-body ranking step
(`migrations.rs`, `retrieval.rs`, `schema_migrations.rs`, `retrieval_smoke.rs`,
`target_spike_corpus.rs`, `legi_canonical_retrieval.rs`, `IMPLEMENTATION_PLAN.md`).
Reviewer: Claude (Opus 4.8), 2026-06-21.

The change is correct, minimal, consistent across production and test probes, and satisfies the
stated intent. Migration 8 is atomic and idempotent, the snippet correctly stays on raw body, and a
focused test proves header-term lexical retrieval. No blocking issues.

## Blocking findings

None.

## What I verified

- **Migration 8 is safe and complete.** It backfills `contextualized_body = body` for any
  `NULL`/blank rows *before* rebuilding the index, then `DROP INDEX IF EXISTS chunks_bm25_idx` +
  `CREATE INDEX … USING bm25 (chunk_id, contextualized_body)`, then records schema 8 — all inside the
  per-migration `BEGIN;…COMMIT;`, so it is atomic and idempotent (re-run is skipped via
  `schema_migrations`). The original index (migration 1, `bm25 (chunk_id, body)`) is correctly the
  one being replaced, and `schema_migrations.rs` now asserts the rebuilt index's `indexdef` contains
  `contextualized_body`.
- **The lexical leg is the only retrieval change.** `hybrid_candidates_json` switches just the match
  predicate (`c.body @@@` → `c.contextualized_body @@@`); scoring stays `paradedb.score(c.chunk_id)`
  (keyed on the index's `key_field`), and everything else (dense leg, RRF fusion, validity filters,
  ordering) is untouched — minimal regression surface.
- **Snippets correctly stay on raw body.** The snippet is `left(regexp_replace(c.body, '\s+', ' ',
  'g'), 280)` — a plain column read, **not** `paradedb.snippet(...)`. So dropping `body` from the BM25
  index does not break snippet generation, and the intent "keep snippets based on raw body" holds.
- **The proof test is well-constructed.** `retrieval_smoke` gives chunk 1240
  `contextualized_body = "Code civil > Article 1240\n…"` and the recipe chunk
  `"Code de cuisine > Article 1\n…"`, then queries the **header-only** term `code civil` (absent from
  both raw bodies) and asserts the civil article ranks first — proving both that a context/header term
  retrieves through `contextualized_body` and that it discriminates correctly (`civil` tips 1240 over
  the cuisine chunk). Both `target_spike_corpus` and `legi_canonical_retrieval` direct BM25 probes
  switch to `contextualized_body` (with header-prefixed fixtures), so all probes match production; a
  grep confirms **no** `body @@@` references remain.
- **No current unsearchability.** `build_article_chunks` (`legi/mod.rs:1088`) sets
  `contextualized_body` to `body` when the hierarchy context is empty, otherwise `context + body`, so
  it is always non-empty (≥ body). Combined with migration 8's backfill, every existing and future
  LEGI chunk has non-empty `contextualized_body` and is covered by the index.
- Plan marks the item Done with an accurate operator note (the migration drops/rebuilds the
  `pg_search` index and temporarily removes lexical coverage during the run). `schema_migrations` and
  `retrieval_smoke` pass locally; `clippy -p jurisearch-storage --all-targets -D warnings` is clean.

## Non-blocking suggestions

1. **Add a non-empty invariant for `contextualized_body` (lexical recall safety).** The *primary*
   lexical field moved from `body` (NOT NULL, always non-empty) to `contextualized_body` (nullable;
   backfilled by migration 8 but not constrained at write time). For LEGI it is provably always
   non-empty, so nothing is currently unsearchable — but note the asymmetry: the dense path already
   *defensively* falls back to `body` for empty `contextualized_body`, while the lexical path now does
   not, so any future/non-LEGI path that emits an empty `contextualized_body` would make that chunk
   **silently invisible to lexical search**. A cheap guard removes the footgun: a
   `CHECK (btrim(contextualized_body) <> '')` (+ NOT NULL), an insert-time fallback to `body`, or
   indexing `COALESCE(NULLIF(contextualized_body, ''), body)`.
2. **Measure the ranking shift before trusting it at scale.** Prepending the hierarchy header to every
   chunk raises BM25 document length and repeats header terms across all chunks in a section (lowering
   their IDF). That is the intended trade-off, but the only evidence is a 2-doc smoke; consider a
   before/after BM25 ranking check on the target-spike corpus (or running the hierarchy eval fixtures
   once a harness exists) to confirm plain body-term queries did not lose precision/recall from the
   added header tokens and longer documents.
3. **Header-only matches return a body snippet without the matched term (deliberate UX choice).** A
   query that matches only via the header (e.g. `code civil`) yields a snippet drawn from `body`,
   which won't contain the query term — potentially confusing. The intent explicitly chose body-based
   snippets, so this is acceptable; consider surfacing the matched hierarchy/context separately in
   results so the match is explainable.
4. Minor: the `target_spike_corpus` fixture duplicates the body text into `contextualized_body` via a
   repeated `CASE`; a one-line comment noting this mirrors production's `header + "\n" + body` shape
   would aid future readers.
