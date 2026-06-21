# Claude Review - Long Article Structural Splitting

Verdict: GO

Reviewed: uncommitted Phase 1.2 long-article structural sub-splitting
(`crates/jurisearch-ingest/src/legi/mod.rs`, `work/03-implementation/IMPLEMENTATION_PLAN.md`).
Reviewer: Claude (Opus 4.8), 2026-06-21.

The change is correct, conservative, and matches the stated intent exactly. Normal articles are
unchanged apart from the version bump; long articles split only on already-preserved structural
newlines, never hard-split a single oversized alinéa, repeat context per chunk, and carry the right
boundary/source-field/version metadata. No blocking issues.

## Blocking findings

None.

## What I verified

- **Intent is fully met.** `build_article_chunks` emits one `article` chunk when the contextualized
  body is within the 6000-char guardrail; otherwise it splits `structural_article_body_units` (the
  non-empty trimmed lines = the `BLOC_TEXTUEL/CONTENU` block boundaries) by greedy packing under the
  same guardrail, tagging chunks `boundary = alinea` (single unit) / `alinea_range` (multiple),
  `source_fields` with `BLOC_TEXTUEL/CONTENU/alinea:{start}-{end}`, `chunking = structural`, and
  `chunk_builder_version = legi_article_structural:v2`. Context (hierarchy + article title) is
  re-prepended per chunk via `contextualized_article_body`.
- **Chunk contract holds for multi-chunk documents.** `validate_for_document` requires
  `chunk_index == array position`, `chunk_id == chunk:{document_id}:{index}`, non-empty body, and
  `chunking == "structural"` — all satisfied: `push_alinea_chunk` passes `chunks.len()` as the index
  (yielding contiguous 0,1,2…) and `build_article_chunk` derives the matching id. There is no
  reassembly/concatenation constraint, so the split passes cleanly. The parse fixture validates, and
  the full `legi::tests` suite (23) + clippy pass.
- **No hard-split of a single oversized alinéa.** If `units.len() <= 1` the function returns a single
  `article` chunk, and in the greedy loop a unit that alone exceeds the guardrail lands in its own
  chunk (the `!current_units.is_empty()` guard prevents flushing an empty batch), producing an
  over-guardrail chunk left for preflight rather than an arbitrary cut — exactly as intended.
- **Alinéa range bookkeeping is correct.** I traced the greedy loop: `current_start` (1-based) and
  `end` (loop index at flush / `units.len()` at final flush) bound each chunk's units; the test
  confirms `1-1`, `2-2`, `3-3` for three one-paragraph chunks, and the range case computes `1-2`,
  `3-4` correctly. No units are lost and no empty chunk is emitted.
- **Lossless for real LEGI bodies.** The body is built with single-`\n` block boundaries and
  whitespace-collapsed content, so `units` (trim + drop-empty) joined by `\n` reconstructs
  `document.body`; the new test asserts `document.body == "{first}\n{second}\n{third}"` and each
  chunk body equals its paragraph.
- **Normal-case output is unchanged except the version.** For a short article every field
  (chunk_id `…:0`, index 0, body, contextualized_body, `boundary=article`, source_fields) is
  identical to before; only `chunk_builder_version` moves v1→v2 (existing test updated). Splitting is
  deterministic/idempotent, so re-parse yields identical chunks. A defensive `chunks.len() <= 1`
  fallback re-emits a single `article` chunk if normalization happens to bring everything under the
  guardrail.

## Non-blocking suggestions

1. **Decide whether to force reprocessing of existing indexes.** The chunk format changed (v2 +
   sub-splitting) but no parser/compatibility version (`LEGI_PARSER_VERSION` / canonical version) was
   bumped, so a resume over an already-ingested archive treats members as `compatible_complete` and
   **skips** them — existing long articles keep their v1 single oversized chunk and short articles keep
   the v1 builder tag until an intentional reprocess. The TEXTELR slice bumped a version to force
   replay for exactly this reason; either do the same here or document that the new chunking applies to
   fresh ingests / intentional rebuilds only.
2. **Cover the two intent-specific branches the test misses.** The new test only exercises
   one-paragraph-per-chunk (`alinea`). Add tests for (a) the **single oversized alinéa** path (one
   over-guardrail unit → one chunk, not hard-split — the core "don't arbitrary-split" guarantee) and
   (b) the **`alinea_range`** path (several small alinéas packed into one chunk). Both are cheap and
   pin behaviors that currently rely on code reading.
3. **Note the body-normalization assumption.** Split chunk bodies are trim+drop-empty normalized and
   `\n`-joined, while the single-chunk path preserves `document.body` verbatim. This is lossless only
   because LEGI bodies already use single-`\n` boundaries with collapsed whitespace; a future
   body-format change introducing blank lines would silently alter split-chunk bodies (nothing
   enforces reassembly == `document.body`). A one-line comment recording that invariant would help.
4. **Minor perf.** The greedy loop rebuilds the candidate string and re-counts chars each iteration
   (≈O(body²/guardrail) worst case). Fine for real articles; tracking a running char count would make
   pathological many-alinéa articles linear. Optional.
5. The 6000-char guardrail is a reasonable Phase-1 proxy (well under bge-m3's ~8k-token window); the
   plan correctly leaves tokenizer-grade limits to the next slice. No action.
