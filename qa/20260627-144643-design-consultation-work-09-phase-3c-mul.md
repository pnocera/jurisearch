# Phase 3C Design Consultation

**Verdict: GO with adjustments.**

The 3C direction is right: keep the proven single-corpus SQL, run it once per active physical generation inside the P3B snapshot, then fuse above the arms. The important corrections are around snapshot path hygiene, pagination proof, and which operations are allowed to use union views. The current P3B code already gives you the right foundation: `ReadSnapshot` is `&mut`, `LocalSnapshot` owns one `REPEATABLE READ READ ONLY` connection, `manifest_default_probes` is snapshot-bound, and search/zone candidates already have `_in_snapshot` entry points.

## 1. Fan-Out Mechanism

Per-arm `SET LOCAL search_path TO <physical_schema>, public` is the right minimal mechanism for 3C. Schema-qualifying the existing SQL would be much more invasive: the query builders reference many tables and public functions/operators, and the current SQL already works correctly under a physical-generation `search_path`.

Adjustment: do not rely on "restoring nothing." `SET LOCAL` changes transaction-local state until another `SET LOCAL` or transaction end. In a P3B request, multiple reads can happen through the same snapshot: structured citation resolution may run before hybrid fallback, zone candidate retrieval may be followed by zone coverage, and builders may call helper reads. If a fan-out arm leaves the path on corpus B, the next generic `read_text` could accidentally read corpus B instead of the default topology.

Use an explicit path API:

```rust
pub trait ReadSnapshot {
    fn read_text(&mut self, sql: &str) -> Result<String, StorageError>; // default path
    fn read_text_for_corpus(&mut self, corpus: &ActiveCorpus, sql: &str) -> Result<String, StorageError>;
    fn active_corpora(&self) -> &[ActiveCorpus];
}
```

For `len > 1`, `read_text` should use the default non-indexed path: `jurisearch_server, public`. `read_text_for_corpus` should set `<corpus.schema>, public` for that one arm. Either set the path before every call, or restore the default path after every arm. The former is simpler and safer.

Repeated `SET LOCAL search_path` inside one `REPEATABLE READ` transaction is correct. It does not change the MVCC snapshot; it only changes name resolution. Keep schema names identifier-quoted through the existing `sql_identifier`.

## 2. Cross-Corpus Fusion

RRF over per-arm ranks is the right cross-corpus merge. Do not reuse per-arm `scores.rrf` as the cross-corpus score: those scores are within-corpus BM25/dense fusion products and are not calibrated across corpora. Treat each corpus arm as a ranked list, assign rank 1..N in that arm, then compute cross score as `sum(1 / (RRF_K + rank_in_arm))`.

Keep the single-corpus path byte-identical. If `active_corpora().len() <= 1`, call the existing SQL path and return its current JSON and cursor format unchanged. For `len > 1`, the fused candidates should use the cross-corpus RRF score in `scores.rrf`, because downstream authority rerank reads that field. If useful, preserve the original arm score under a new diagnostic key such as `scores.local_rrf`, but do not make authority rerank operate on local scores.

Tie-break with a stable tuple, not just an id: `(cross_rrf desc, corpus, document_id/chunk_id)`. Add the corpus id to each multi-corpus candidate payload or at least to the cursor/diagnostics. The question assumes stable ids belong to one corpus; make that assumption explicit in tests, because duplicate ids across corpora would otherwise make cursoring and dedupe ambiguous.

A single SQL `UNION ALL` global ranking query is not the right 3C slice. It would either hit union views or force a much larger SQL generator. Rust-side fan-out/fusion is the least disruptive way to prove the invariant that hot search uses physical generation indexes.

## 3. Pagination

"Overfetch each arm + fuse + keyset on `(cross_rrf_score, id)`" is directionally right, but a fixed `limit * N` depth is not a proof. It can miss later pages or skew a page boundary when the cursor is deep.

For exact rank-only RRF with one corpus occurrence per candidate, a safe depth is cursor-aware:

1. First page: fetch at least `top_k + 1` candidates per arm.
2. Cursor page: derive the previous rank boundary from the cursor score when possible (`rank ~= 1 / score - RRF_K` for single-arm rank-score), then fetch at least `boundary_rank + top_k + 1` per arm.
3. After fusion, filter after the cursor tuple and return the next page.

An adaptive implementation is more robust: start with a depth, fuse, find the returned page boundary, and increase arm depth until every arm's maximum unseen rank score is lower than the boundary score, including tie-break handling. Without that proof, call the cursor "best effort" rather than "stable"; the plan wants stable pagination, so implement the proof or the cursor-aware bound.

Use a multi-corpus cursor prefix instead of overloading current cursors silently, for example:

```text
mc:<group>:<cross_score>:<corpus>:<id>
```

Keep existing `doc:<score>:<document_id>` and `<score>:<chunk_id>` cursors for single-corpus byte parity. Reject cursor/topology mismatches clearly.

## 4. Fingerprint Semantics

Fail-closed across all touched corpora is the right 3C behavior. For dense/hybrid main search, "touched" should mean every active corpus included in the fan-out. If any active corpus has a `corpus_state.embedding_fingerprint` different from the query embedding fingerprint, error before retrieval. BM25 fan-out has no fingerprint constraint.

Do not implement per-fingerprint embedding in 3C. It would change embedder lifetime, cost, cache semantics, and result comparability. A later phase can group corpora by fingerprint and embed once per fingerprint, but 3C should prove the fail-closed invariant.

One source-backed hazard: `index_manifest` dense keys are still global. `generations.rs` explicitly notes that true per-corpus dense manifest isolation is deferred. Therefore, do not use `index_manifest['embedding']` as the compatibility authority in 3C. Use `ActiveCorpus.fingerprint` for chunk dense compatibility. Treat `manifest_default_probes` as tuning only; if it is stale from another corpus, correctness is still preserved but performance/probe tuning may be imperfect.

## 5. By-ID And Non-Indexed Operations

Do not fan out `fetch`, `cite`, `context`, and `related` in the initial 3C slice. They are non-indexed/by-id or exact-lookup surfaces, and the old runtime already documents the intended split: union views are correct for non-indexed reads, while hot indexed search needs physical-generation fan-out.

For `len > 1`, make default `read_text` use `jurisearch_server, public`; then these operations can keep their existing snapshot-bound SQL and read through the stable union views. This is the minimal correct behavior.

Routing by stable id to an owning corpus can be deferred. It would require an ownership lookup or metadata convention that does not exist in the current query layer. Add a test that the same snapshot can `fetch` a document from each of two active corpora through the union view.

Structured citation resolution inside `search` is also non-indexed exact lookup. Let it use the default union-view path. If it misses and the request falls back to conceptual search, the hybrid fallback must use the physical fan-out path.

## 6. Zone, Authority, And Filters

Decision filters belong inside each physical arm, exactly where the SQL applies them today. They must constrain the per-corpus candidate pool before local ranking and local limiting, otherwise out-of-filter rows can consume arm slots.

Authority rerank belongs after cross-corpus fusion, not per arm. The authority helper assumes an already relevance-sorted window and uses `scores.rrf`; in multi-corpus that should be the cross-corpus RRF score. Keep the existing first-page-only restriction for authority cursors. Per-arm authority rerank would let publication metadata reorder candidates before the global relevance merge and would change semantics.

`search --zone` must not silently run over union views in a multi-corpus snapshot. You have two acceptable 3C cuts:

1. Implement zone fan-out with the same mechanism as main search, over corpora whose per-arm zone coverage has indexed units. BM25 zone search can be fused by rank like main search. Dense/hybrid zone search should fail unless every touched zone corpus has complete zone embeddings under the query fingerprint.
2. If that is too much for 3C, make multi-corpus `--zone` fail closed with a clear "multi-corpus zone fan-out deferred" error, and add that as the verified zone behavior.

Given the plan explicitly lists `--zone` behavior as regression-sensitive, do not leave it accidentally using `zone_candidates_in_snapshot` against `jurisearch_server` union views. Either fan it out or reject it.

The current zone readiness path reads `zone_retrieval_coverage_in_snapshot`, whose `embedding_manifest` comes from global `index_manifest`. That is not a per-corpus compatibility authority. If you implement multi-corpus dense zone in 3C, do the readiness/coverage check per arm and be explicit that global manifest data is only advisory until per-corpus dense manifest keys exist.

## 7. Minimal 3C Slice

The smallest slice that proves the 3C invariant is:

1. Lift `LocalSnapshot::open`'s `len > 1` refusal.
2. Add explicit default-path and per-corpus read methods to `ReadSnapshot`.
3. For `len > 1`, default `read_text` uses `jurisearch_server, public`; per-arm reads use `<physical_generation>, public`.
4. Keep `len <= 1` search byte-identical.
5. Implement multi-corpus fan-out/fusion for main `hybrid_candidates_in_snapshot` across `Bm25`, `Dense`, and `Hybrid`, for both chunk and document grouping.
6. Implement all-active-corpus fingerprint preflight for dense/hybrid.
7. Implement stable multi-corpus pagination with a cursor-aware or adaptive arm depth.
8. Apply decision filters per arm.
9. Apply authority rerank only after cross-corpus fusion, and keep it first-page-only.
10. Give `--zone` an explicit behavior: real zone fan-out or fail-closed unsupported; no union-view hot path.

Test this on a two-corpus harness:

1. BM25 search returns fused candidates from both corpora and the arm plan/path references physical generation schemas, not `jurisearch_server`.
2. Dense/hybrid with one mismatched corpus fingerprint errors before retrieval and returns no partial results.
3. Pagination returns stable non-overlapping pages across arms.
4. Decision filters are applied inside arms.
5. Authority rerank is applied after fusion and disables cursor paging as today.
6. Fetch/cite or fetch/context can read rows from both corpora through the union-view default path.
7. Zone multi-corpus behavior is explicitly tested, either fan-out or fail-closed.

Deferrable: per-fingerprint embedding, owner-corpus routing for by-id reads, typed P4 search builders, per-corpus dense manifest keys/default probes, and full zone dense multi-corpus if you choose the fail-closed zone cut.

## 8. Additional Risks

The current `ReadSnapshot` trait has only one read method, so adding fan-out by mutating `search_path` without changing the trait will be fragile. Make the path choice part of the API.

Do not pass the current per-arm `after_cursor` into each SQL arm for multi-corpus search. Existing cursors are local-score cursors; multi-corpus cursors are cross-fusion cursors. For multi-corpus, fetch per-arm prefixes and apply cursor filtering after Rust fusion.

The response shape needs one small multi-corpus addition: a visible or diagnostic corpus identifier. Without it, users cannot explain mixed results, and the cursor cannot be unambiguous under ties.

Be careful with `manifest_default_probes`: today it reads global `index_manifest`. That is acceptable as a tuning fallback, but it is not a multi-corpus readiness or compatibility proof.

Net: implement 3C as a physical-arm search primitive plus Rust fusion, not as a general rewrite of retrieval. The design is sound once `search_path` state is explicit and pagination is made provable rather than heuristic.
