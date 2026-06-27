# P3C working notes — codex design GO-with-adjustments (qa/20260627-144643)

Implement 3C as a **physical-arm search primitive + Rust fusion**, NOT a retrieval rewrite. Foundation
from P3B is right (`ReadSnapshot` is `&mut`, one REPEATABLE READ READ ONLY conn, `_in_snapshot` cores).

## Binding adjustments

1. **Explicit path API** (don't rely on "restoring nothing" — SET LOCAL persists till next SET/txn-end;
   a fan-out arm leaving path on corpus B would poison the next `read_text`):
   ```rust
   pub trait ReadSnapshot {
       fn read_text(&mut self, sql) -> ...;                                 // DEFAULT path
       fn read_text_for_corpus(&mut self, corpus: &ActiveCorpus, sql) -> ...; // <schema>, public
       fn active_corpora(&self) -> &[ActiveCorpus];
   }
   ```
   - len>1: default `read_text` = `jurisearch_server, public` (union views, for by-id/non-indexed).
     `read_text_for_corpus` = `<corpus.schema>, public`. SET the path before EVERY call (simpler/safer).
   - len<=1: unchanged (pin the one gen, or public). Repeated SET LOCAL is correct (no MVCC change).
     Quote schema via `sql_identifier`.
2. **Fusion = RRF over per-arm ranks** (NOT per-arm `scores.rrf` — uncalibrated cross-corpus). Each arm =
   ranked list, rank 1..N, cross score = `sum(1/(RRF_K + rank_in_arm))`.
   - len<=1: BYTE-IDENTICAL (existing SQL path + cursor).
   - len>1: fused candidates put cross-corpus RRF in `scores.rrf` (authority rerank reads it); optionally
     keep local under `scores.local_rrf`. Tie-break `(cross_rrf desc, corpus, doc_id/chunk_id)`. Add
     corpus id to multi-corpus candidate payload + cursor/diagnostics. NOT a single UNION ALL query.
3. **Pagination — provable, not heuristic** (fixed limit*N is NOT a proof):
   - First page: ≥ top_k+1 per arm. Cursor page: derive prev rank boundary from cursor score
     (`rank ≈ 1/score - RRF_K` single-arm), fetch ≥ boundary_rank + top_k + 1 per arm.
   - Adaptive (more robust): increase arm depth until every arm's max-unseen-rank-score < page-boundary
     score (incl. ties). Without the proof, label cursor "best effort" — plan wants STABLE, so do the proof.
   - Multi-corpus cursor PREFIX `mc:<group>:<cross_score>:<corpus>:<id>`; keep existing
     `doc:<score>:<id>` / `<score>:<chunk_id>` for single-corpus byte parity. Reject cursor/topology mismatch.
   - Do NOT pass the existing per-arm `after_cursor` into each SQL arm (those are local-score cursors);
     fetch per-arm prefixes, apply cursor filtering AFTER Rust fusion.
4. **Fingerprint — fail closed across ALL touched corpora**: every active corpus in fan-out must have
   `corpus_state.embedding_fingerprint == query fp`, else error BEFORE retrieval (no partial). BM25 = no
   constraint. Use `ActiveCorpus.fingerprint` (NOT `index_manifest['embedding']` — global, not per-corpus).
   `manifest_default_probes` = tuning fallback only (global, advisory). No per-fingerprint embedding in 3C.
5. **By-ID ops (fetch/cite/context/related) do NOT fan out in 3C** — non-indexed/by-id. For len>1 they use
   the DEFAULT `read_text` (= `jurisearch_server, public` union views). Owner-corpus routing deferred.
   Structured-citation resolution in `search` = non-indexed exact lookup → default union-view path; on
   MISS, the hybrid fallback uses the physical fan-out path.
6. **Zone / authority / filters**:
   - Decision filters INSIDE each arm (where the SQL applies them today) — before local ranking/limiting.
   - Authority rerank AFTER cross-corpus fusion (operates on cross-corpus `scores.rrf`), keep
     first-page-only (disables cursor paging as today).
   - `--zone` must NOT silently hit union views. 3C cut: **multi-corpus `--zone` fails closed** with a
     clear "multi-corpus zone fan-out deferred" error (zone is Cassation-only single-corpus in practice);
     single-corpus zone stays byte-identical. (Alt: full zone fan-out — deferred.)

## Minimal slice (the 10 steps)
1 lift len>1 refusal · 2 add default+per-corpus read methods · 3 len>1 default=jurisearch_server,public,
arms=<gen>,public · 4 len<=1 byte-identical · 5 fan-out/fusion for `hybrid_candidates_in_snapshot`
(Bm25/Dense/Hybrid × chunk/document) · 6 all-corpus fp preflight (dense/hybrid) · 7 stable pagination
(cursor-aware/adaptive depth) · 8 filters per arm · 9 authority after fusion, first-page-only · 10 `--zone`
explicit (fail-closed multi-corpus).

## 2-corpus harness tests
1 bm25 fused from both + plan references physical gen schemas (NOT jurisearch_server) · 2 dense/hybrid one
fp-mismatched → error before retrieval, no partial · 3 pagination stable non-overlapping across arms · 4
filters applied inside arms · 5 authority after fusion + disables paging · 6 fetch/cite/context reads from
BOTH corpora via union-view default path · 7 zone multi-corpus explicit (fail-closed).

Deferrable: per-fingerprint embedding, owner-corpus by-id routing, typed P4 builders, per-corpus dense
manifest keys/probes, full zone dense multi-corpus.

## Precise fan-out algorithm (RRF_K = 60, `retrieval/sql.rs:172`/`types.rs:25`)

`hybrid_candidates_in_snapshot(snapshot, query)` DISPATCHES on `snapshot.active_corpora().to_vec()`:
- len<=1 → `hybrid_candidates_single` (the CURRENT body — extract it; byte-identical, via `read_text`).
- len>1 → `hybrid_candidates_fanout(snapshot, &corpora, query)`.

EXTRACT `build_hybrid_sql(query) -> String` from the current inline body (so an arm can build SQL without
running it). `hybrid_candidates_single` = `build_hybrid_sql` + `read_text`.

`hybrid_candidates_fanout`:
1. **Preflight** (dense/hybrid only): for each corpus, `query.embedding_fingerprint == Some(c.fingerprint)`
   else `StorageError::Retrieval { "embedding_fingerprint_mismatch: corpus <c> fp <c.fp> != query <qfp>;
   multi-corpus dense fails closed" }`. BM25 → no check.
2. **Depth (cursor-aware)**: `top_k = query.limit` (caller already passes top_k+1). Parse the optional
   multi-corpus after_cursor → `(cursor_score, cursor_corpus, cursor_id)`. `implied_rank = (1/cursor_score
   - RRF_K).ceil()`. `depth = if cursor { implied_rank + top_k + 1 } else { top_k + 1 }` (proof: top_k of a
   k-way merge of rank-sorted lists ⊆ union of each list's top-`depth`; cursor page needs ranks up to
   cursor_rank+top_k in the worst single-arm case).
3. **Per arm**: `arm_query = HybridCandidateQuery { after_cursor: None, limit: depth, ..*query }`;
   `sql = build_hybrid_sql(&arm_query)`; `json = snapshot.read_text_for_corpus(c, &sql)?`; parse
   `candidates[]`; candidate at index i has `rank_in_arm = i+1`, `cross = 1.0/(RRF_K + rank)`.
4. **Fuse**: collect all candidates (each id is in ONE corpus — codex assumption, assert in tests); sort by
   `(cross desc, corpus, id)`. Set `scores.rrf = round(cross,8)`, keep `scores.local_rrf = old rrf`, add
   `"corpus": c.corpus`, set `cursor = "mc:<group>:<cross>:<corpus>:<id>"` (group = chunk|document).
5. **Cursor filter**: if cursor, retain only candidates strictly AFTER `(cursor_score, cursor_corpus,
   cursor_id)` in the sort order.
6. **Page**: `truncate(query.limit)`. Build response `{ query, retrieval_mode, as_of, group_by, limit,
   candidates }` — SAME shape as single-corpus + `corpus` per candidate + the `mc:` cursor.

**Cursor plumbing**: add `RetrievalCursor::MultiCorpus { score, corpus, id }` (storage types.rs); CLI
`parse_search_cursor` parses the `mc:` prefix → a `ParsedSearchCursor::MultiCorpus` → maps to the storage
cursor. The CLI search dispatch must accept the mc cursor for ANY group_by (it encodes its own group).
The CLI `apply_search_response_envelope` already truncates to top_k + reads `candidate.cursor` for
`next_cursor` — works unchanged with the mc cursor. Authority rerank reads `scores.rrf` (= cross score) →
applied AFTER fusion automatically (it runs in the CLI envelope, post-storage). Decision filters are
already INSIDE each arm's SQL (per the existing query). Keep single-corpus byte-identical (no `corpus`
key, no `mc:` cursor, `scores.rrf` = local).

**Zone (3C cut = fail-closed)**: in `zone_candidates_in_snapshot` (or the CLI zone adapter), if
`active_corpora().len() > 1` → `StorageError::Retrieval { "multi-corpus zone fan-out is deferred; --zone
requires a single active corpus" }` (single-corpus zone byte-identical). Decide placement: cleanest in the
CLI `zone_search_payload` (check `snapshot.active_corpora().len()` after begin_snapshot).

## 2-corpus harness (build a REAL 2nd corpus, e.g. seed inpi docs+chunks+embeddings, activate inpi gen)
Tests (storage + CLI): bm25 fused from both arms + plan hits physical gen schemas (EXPLAIN has no
`jurisearch_server`); dense one-fp-mismatched → error pre-retrieval, no partial; pagination stable
non-overlapping across arms (page1 ∪ page2 = expected, no dup); each candidate carries `corpus`;
fetch/context by-id reads from BOTH via the union-view default path; multi-corpus `--zone` → fail-closed.

## Status
- [x] API: `ReadSnapshot::read_text_for_corpus` + len>1 default=`jurisearch_server,public`; refusal LIFTED
  (`query.rs`). P3B refusal test → `a_multi_corpus_snapshot_opens_and_resolves_every_active_corpus`.
  Tree GREEN (single-corpus unchanged). UNCOMMITTED (P3C is one review gate; commit only when complete).
- [ ] `build_hybrid_sql` extraction + dispatch + `hybrid_candidates_fanout` (algorithm above).
- [ ] `RetrievalCursor::MultiCorpus` + CLI `parse_search_cursor` mc: + envelope.
- [ ] all-corpus fp preflight; per-arm filters (already in SQL); authority post-fusion (already in CLI).
- [ ] zone multi-corpus fail-closed.
- [ ] 2-corpus harness tests. Then codex review (disclose: zone fail-closed cut, by-id non-fanout).
