# Claude Review — Phase 1.2 Context Reconstruction (`context` command)

- Step reviewed: uncommitted Phase 1.2 `context` implementation
- Reviewer: Claude (Opus 4.8), 2026-06-21

The implementation is correct, regression-free, and satisfies the plan's functional intent
(ancestry + same-section siblings + `--as-of`). Tests pass and `clippy --all-targets -D warnings`
is clean. There is one real scalability/contract concern (unbounded, full-scan sibling lookup) that
I'm treating as a strong non-blocking recommendation consistent with how this project defers
corpus-scale performance, plus a few minor items.

## What I verified (correct)

- **Temporal/visibility semantics are sound.** `effective_as_of = COALESCE(as_of, target.valid_from,
  CURRENT_DATE)`; `visible_target` returns the target unconditionally when `--as-of` is absent
  (`{as_of} IS NULL → true`) and otherwise applies the half-open `[valid_from, valid_to)` window —
  the same convention used elsewhere (`retrieval.rs`). When `--as-of` predates the target's validity
  the target is empty, which the CLI turns into a clean `no_results` (`main.rs context_payload`,
  `target.is_null()` check). Verified by the `before_validity` storage assertion.
- **Sibling matching is correctly scoped.** Siblings require same `source`/`kind`, a different
  `document_id`, an **exact** `hierarchy_path` array match, the target's path being a non-empty array
  (guarding against "everything with empty hierarchy is a sibling"), and validity at
  `effective_as_of`. The storage test confirms the same-section sibling (1241) is included, the
  different-section article (Titre V) is excluded by path, and the future version (valid_from 2025)
  is excluded at `as_of=2024`. The `--as-of`-absent default correctly reconstructs siblings at the
  target's `valid_from`.
- **Ancestry** is derived from the target's `hierarchy_path` via `jsonb_array_elements_text WITH
  ORDINALITY` (`depth = ordinality-1`, section titles only; the article itself stays in `target`).
- **Hierarchy source + fallback** is consistent for both target and siblings: prefer the chunk's
  `chunks.hierarchy_path` (the column materialized in migration 6), fall back to
  `canonical_json->'hierarchy_path'`, else `[]`.
- **Safety / no injection:** `document_id`, `as_of`, and `requested_as_of` go through
  `sql_string_literal`; `include_siblings` is a controlled `true`/`false` literal. `--as-of` is
  shape-validated before the query (`validate_as_of`), and a malformed value (`20240101`) is rejected
  as `bad_input` (CLI test, exit code 2).
- **No regression.** This only flips `context` from not-implemented to implemented (CLI dispatch +
  JSONL session). No existing query/command changed; `ensure_query_readiness(Fetch)` gates it like
  `fetch` (correctly not requiring embedding coverage). Schema/contract updates are additive.
- Both named tests pass locally; clippy clean across the three crates.

## Non-blocking suggestions

1. **(Primary) Sibling lookup is a full scan and is unbounded.** `sibling_candidates`
   (`retrieval.rs`) joins the target against **all** same-`source`/`kind` documents
   (`JOIN documents d ON d.source=t.source AND d.kind=t.kind`), runs a per-document `LATERAL` chunk
   lookup, and filters on `sibling_path.hierarchy_path = t.hierarchy_path`. There is no index on
   `hierarchy_path` (only `documents_kind_idx` / `chunks_document_idx`), so at corpus scale a single
   `context --siblings` call scans every LEGI article and does one indexed chunk lookup each — likely
   multi-second — and the final `jsonb_agg` of siblings has **no `LIMIT`**, so a large section
   returns thousands of objects (contradicting the plan's "compact … sibling summaries"). This is an
   interactive, agent-facing command, so it matters more than the batch-backfill perf items deferred
   earlier. Before corpus-scale interactive use: add a `LIMIT` (with a truncation/`sibling_count`
   signal — `sibling_count` is already computed separately, which is good), and either index
   `chunks.hierarchy_path` / materialize a section key on `documents`, or resolve siblings via the
   section relationship instead of a path-equality full scan. The default (no `--siblings`) path
   short-circuits on `WHERE false` and is unaffected. Worth noting this limitation explicitly in the
   plan's Remaining list, which currently does not mention it.
2. **Sibling identity is title-path equality, not section UID.** Two genuinely distinct sections with
   coincidentally identical full title paths would merge. Low risk given the path includes the code
   name and every level, but matching on a stable section `source_uid` (when available) would be more
   robust.
3. **`--as-of` validation is shape-only.** `is_iso_date_literal` accepts well-formed-but-invalid
   dates (e.g. `2020-13-45`), which then fail as a raw Postgres `::date` cast error
   (`storage_error_object`) rather than a clean `bad_input`. Consider semantic validation (reuse the
   ingest `validate_date` month/day logic); it also de-duplicates the third copy of date-shape logic.
4. **Minor test gaps.** Not directly covered: `as_of=None` **with** `siblings=true` (the
   default-to-`valid_from` sibling path), the empty-`hierarchy_path` → no-siblings guard, and the
   `canonical_json` hierarchy fallback for a document with no chunk row. All are exercised indirectly
   or by reading, but a focused assertion each would lock them.

## Verification performed

- Read the full `context_documents_json` SQL and traced the `target`/`visible_target`/
  `sibling_candidates` CTEs, the `effective_as_of`/visibility logic, ancestry derivation, and the
  JSON shape; cross-checked the CLI `context_payload` (not-found/invalid-date handling, readiness
  gate), the JSONL `session_context_payload`, the `ContextArgs` clap struct, and the
  contract/schema additions.
- Confirmed there is no index supporting the sibling `hierarchy_path` equality and that the sibling
  aggregate has no `LIMIT`.
- Ran `cargo test -p jurisearch-storage --test retrieval_smoke` (2 passed),
  `cargo test -p jurisearch-cli context_returns_hierarchy_and_siblings_from_existing_index`
  (1 passed), and `cargo clippy -p jurisearch-storage -p jurisearch-cli -p jurisearch-core
  --all-targets -- -D warnings` (clean).

Verdict: GO
