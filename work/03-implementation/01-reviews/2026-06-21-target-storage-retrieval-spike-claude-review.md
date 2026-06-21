VERDICT: GO

# Review — Target storage retrieval spike (`2951c72`)

The spike achieves its purpose: it builds the canonical hybrid candidate query, scales the fixture to the DESIGN §13.3 target shape (50k LEGI + 10k Judilibre = 60k docs), and exercises BM25 + pgvector + RRF fusion + temporal filtering + stable JSON end-to-end. The retrieval SQL is correct and injection-safe, test gating is appropriate, and marking the 0.3 spike checklist complete is justified. Verdict is GO; the realism caveats below are non-blocking but should be recorded so the latency figure is not over-trusted.

## What I verified

**Retrieval SQL (`retrieval.rs`) — correct.**
- All five user inputs pass through `sql_string_literal`; `{lexical_limit}/{dense_limit}/{limit}` are `u32`. No injection surface.
- `SET ivfflat.probes = 4` and the `WITH` query are sent in one `execute_sql`/`psql -c` batch, so the GUC applies to the same session — correct.
- Dense path over-fetches a `dense_pool` (4× `dense_limit`), applies the validity post-filter, then **re-ranks** with a fresh `row_number()` so RRF sees gap-free ranks. `fused` uses `min(...)` over a `UNION ALL` with NULL placeholders — aggregate NULL-skipping yields the right per-component rank. RRF `1/(60+rank)` is standard. 1:1 joins (chunk_id/document_id PKs) so no row fan-out.
- Temporal predicate is half-open `[valid_from, valid_to)` (`valid_from <= as_of` inclusive, `valid_to > as_of` strict) — the right convention for same-day version changes; worth confirming against the 0.2 temporal fixtures.

**Test gating — correct.** `target_spike_corpus.rs` is `#[ignore]` with a reason, and `discover_pg_config` only hard-errors when `JURISEARCH_REQUIRE_PG_EXTENSIONS` is set (which Codex's run used). The 500 ms assertion matches the acceptance criterion. The `retrieval_smoke.rs` extension (now asserting the hybrid JSON shape, ranks, and `serde_json` parseability) is a real improvement. Cargo/Cargo.lock changes are just the `serde_json` dev-dependency.

**Plan status — justified.** Across commits, the 0.3 task list and acceptance items (lifecycle, loopback/socket-only binding, single-writer + advisory locks, crash-recovery/clean-shutdown, migrations, platform policy, target-scale BM25+vector retrieval, <500 ms warm JSON) are all covered. The "move to 0.4 unless review finds a gap" wording is appropriately hedged.

## Suggestions / Recommendations (non-blocking, may apply without re-review)

1. **Latency figure is a floor, not a realistic benchmark — record this caveat next to the numbers in IMPLEMENTATION_PLAN.md.** The dense latency (101.91 ms / warm 127.32 ms) is measured over a *degenerate* vector distribution: every chunk gets one of only **two** distinct 1024-d vectors (target `[1,0,…]` vs decoy `[0,1,…]`). IVFFlat over ~2 distinct points is structurally trivial, so the dense number does not represent real distance/probe work over 60k varied embeddings. The `< 500 ms` acceptance is met structurally but is not performance-validated. Recommend either noting "synthetic/degenerate vectors; latency is a plumbing floor" in the plan, or seeding pseudo-random per-chunk vectors before relying on this for capacity planning.

2. **One chunk per document understates the dense index.** `chunks`/`chunk_embeddings` hold 60k rows because each fixture doc has exactly one chunk. A real 60k-doc corpus (especially Judilibre decisions) chunks into many more embeddings, so the IVFFlat row count — and dense latency — is undersized. Worth a one-line caveat.

3. **Temporal filter is a no-op in the fixture.** All docs are `valid_from='2024-01-01'`, `valid_to` NULL, queried at `as_of='2024-06-01'`, so nothing is excluded and the dense post-filter / 4× over-fetch headroom is never stressed. Consider seeding some expired/future versions so the validity windows and the over-fetch factor are actually exercised.

4. **Dense uses post-filtering; lexical uses pre-filtering.** This is the standard ANN trade-off, but the `4×` over-fetch is a magic constant: if >75% of a query's top dense neighbors are temporally excluded, valid results silently drop. Document the assumption (or make the factor a named constant) so it isn't lost.

5. **Production path should not shell out to `psql` per query.** `hybrid_candidates_json` → `execute_sql` → spawns `psql` (fork + connect) on every call, which is part of the measured 127 ms. The runtime already holds a libpq `postgres::Client` for the advisory lock; real serving should reuse a pooled client. Fine for a spike — flag for the serving layer.
