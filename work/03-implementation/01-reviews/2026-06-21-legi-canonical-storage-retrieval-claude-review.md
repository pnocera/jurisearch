# Review — Add canonical LEGI storage retrieval slice (eafe84c)

Verdict: GO

Scope reviewed: commit `eafe84c` against `IMPLEMENTATION_PLAN.md` §0.6 and the current
storage/ingest code. This is the first 0.6 slice (storage projection + retrieval over a small
official LEGI subset); CLI wiring and live embedding are explicitly deferred. Crate builds clean
(`cargo check`/`clippy -p jurisearch-storage --tests`).

## Why GO

- **No provenance loss.** `insert_legi_documents` stores the full `serde_json::to_string(document)`
  in `documents.canonical_json`, so every field not given a dedicated column
  (`source_status`/`source_nature`/`source_article_type`, `source_archive`, `source_member_path`,
  `hierarchy_path`, and per-chunk `contextualized_body`/`chunking`/`boundary`) is recoverable. The
  smoke verifies the `source_member_path` round-trip out of `canonical_json`.
- **Validation gate.** Each document runs `validate()` before projection, so malformed `source`,
  `kind`, ids, dates, hashes, and chunk invariants fail before any write.
- **Temporal filtering is correct and consistent.** `valid_from <= as_of` / `valid_to > as_of`
  (exclusive) matches the emitted `"to_exclusive": true`. The `2999-01-01` sentinel is normalized to
  `NULL` valid_to upstream, so in-force documents pass `valid_to IS NULL`. Lexical applies the filter
  inline (exact prefilter); dense over-fetches (×4) then filters and re-ranks gap-free; the final
  `limited` re-join cannot reintroduce future versions because every fused chunk already passed a
  temporal filter. The smoke's `as-of=previous-year` case confirms no future-version leak.
- **SQL safety.** Writes use parameterized prepared statements (`$1..$n`, with `::text::date`,
  `::text::jsonb`, `::text::vector` casts on bound params). Reads interpolate only via
  `sql_string_literal` (correct under the default `standard_conforming_strings=on`) and
  integer/`enumerate()` ordinals. No injection vector found.
- **RRF is sound.** `UNION ALL` + `GROUP BY min(rank)` + `1/(60+rank)`, gap-free ranks on both arms.
- **Edges/graph.** `relation → edge_kind`, nullable `to_document_id` (publisher candidates stay
  unresolved), full edge JSON in `payload`. `edge_id` is a hash over `from_document_id|index|...`, so
  it is globally unique — the test's exact `count(*) == report.publisher_edges` assertion is robust.
- **Transaction ordering.** documents+chunks+edges commit in one tx; embeddings insert in a separate
  tx afterward, so the `chunk_embeddings → chunks` FK is satisfied.
- **Retrieval contract matches the acceptance list:** compact ids, citation, title, source_url,
  snippet, validity block, scores, cursor (search); full body + chunk bodies/provenance (fetch).
- **Test is a real smoke,** not a fixture: parses the official archive, asserts zero parse errors,
  lexical hit, hybrid top == target, contract shape, as-of leak prevention, fetch full text + chunk
  fingerprint, edge count, and member-path provenance. Plan acceptance is updated honestly
  (Done / Remaining / Partially met). Default archive is the requested
  `Freemium_legi_global_20250713-140000.tar.gz`, overridable via `JURISEARCH_LEGI_ARCHIVE`.

## Suggestions (non-blocking)

- **S1 — Cursor cannot drive pagination.** `cursor = chunk_id`, but results order by
  `(fused_score DESC, chunk_id)`. A chunk_id-only token can't resume a keyset scan; the plan/commit
  call these "stable cursors." Fine while no paginator exists — tighten to a `(fused_score, chunk_id)`
  cursor when the CLI wires paging.
- **S2 — Redundant score fields.** `lexical_rank`/`dense_rank`/`fused_score` appear both top-level and
  nested under `scores` (where `rrf` duplicates `fused_score`). Collapse to one location before
  consumers bind to both shapes.
- **S3 — No ANN index.** `chunk_embeddings.embedding` has no ivfflat/hnsw index, so
  `SET ivfflat.probes = 4` is a no-op and dense search is a full scan. Harmless for 12 rows but the
  probes line is misleading; fold index creation into the dense rebuild follow-up.
- **S4 — Fingerprint can diverge.** `chunks.embedding_fingerprint` (set in projection) and
  `chunk_embeddings.embedding_fingerprint` (set in the embed insert) are independent and unchecked.
  Cross-validate when the embed path is owned end-to-end.
- **S5 — BM25 query DSL passthrough.** `c.body @@@ {query_text}` hands raw text to ParadeDB's query
  parser. Test inputs are alphabetic-only; arbitrary CLI text could include DSL metacharacters
  (`:`, `+`, `-`, `"`). Plan query sanitization/escaping at the CLI layer.
- **S6 — Data-dependent test assertion.** `top["chunk_id"] == target.chunks[0]` holds because the
  default sample is 1 chunk/article; a multi-chunk target (all target chunks share the same decoy/
  target vector) could let a non-zero index win on fused score. Prefer asserting `document_id` as the
  primary invariant, or pick a single-chunk target explicitly, to keep this robust across archives.
- **S7 — No fast unit coverage** for the pure paths (`fetch_documents_json` empty-list early return,
  `sql_string_literal` use in the VALUES builder). Cheap to add; everything else is gated behind real
  data + PG.
- **S8 — fetch drops/duplicates ids silently.** Unknown ids are omitted and duplicate ids produce
  repeated rows via the VALUES join. Acceptable now; document or de-dup when the CLI consumes it.
- **S9 — `contextualized_body` is not a first-class chunk column.** It's the natural dense-embed text
  and is only recoverable from `canonical_json`; confirm that's the intended source when wiring live
  embeddings.
