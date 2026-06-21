# Claude Review — LEGI Backfill Maintenance Command

Verdict: GO

- Commit reviewed: `46b12bc` "Add LEGI hierarchy backfill command"
- Baseline: `b6558d6`
- Reviewer: Claude (Opus 4.8), 2026-06-21

## Findings

No correctness, data-loss, regression, or missing-test blockers. Findings below are ordered by
severity; all are non-blocking.

### Low — test fixture edge payload does not mirror production shape
`crates/jurisearch-cli/tests/cli_contract.rs:1035-1041` inserts the `graph_edges.payload` with
`"debut"`/`"fin"` as **top-level** keys:
`{"source_tag":"LIEN_SECTION_TA","to_source_uid":"...","debut":"1804-03-21","fin":"2999-01-01"}`.
Production serializes link attributes as a nested array —
`"attributes":[{"key":"debut","value":"..."}]` (from `GraphEdgeAttribute`/`collect_attributes` in
`crates/jurisearch-ingest/src/legi/mod.rs`), and the temporal anchor reader
`hierarchy_backfill_anchor` (`crates/jurisearch-storage/src/projection.rs`) only inspects
`payload.attributes[]`. With the top-level shape, anchor extraction finds no `debut`, falls back to
the article `valid_from` (`1804-02-21`), which does **not** fall inside the section window
(`valid_from 1804-03-21`), so selection falls through to "only/last candidate" and still enriches.
The test therefore passes, but it does not exercise the attributes-based anchor path and could mask
a future regression in `to_source_uid`/`debut` parsing. The JOIN keys (`to_source_uid`,
`source_tag`) are top-level in production too, so those are fine. Recommend aligning the fixture's
edge payload with the real `attributes[]` shape. Non-blocking: the command-wiring behavior under
test (enrichment applied, embeddings invalidated, no accounting writes) is validated correctly.

### Low — new test does not assert idempotency
The test verifies a single invocation. The full backfill is idempotent (it only rewrites documents
when the metadata path is strictly richer, and re-runs converge to 0 updates / 0 invalidations —
covered at the storage layer in earlier commits), but a one-line second-invocation assertion
(`documents == 0`, `invalidated == 0`) would lock that guarantee at the command contract level too.

### Informational — operator must re-embed; output gives only an implicit signal
The command clears chunk `embedding_fingerprint`s and deletes `chunk_embeddings` rows for changed
documents, so the dense index is stale afterward. This is safe in practice because
`ensure_query_readiness` (`crates/jurisearch-cli/src/main.rs:1642-1676`) gates `search` on embedding
coverage, so a post-backfill `search` fails closed with "embedding coverage gate is incomplete"
rather than serving stale results, and the `hierarchy_backfill_invalidated_embeddings` counter
signals the need. Consider adding an explicit "re-run `ingest embed-chunks`" hint to the JSON (or
the plan/runbook) to make the recovery step unambiguous.

### Informational — full-corpus memory/scale
The command runs the unscoped `backfill_legi_article_hierarchy_from_metadata`, which loads all
candidate `article × edge × section` rows into memory. This is the intended explicit maintenance
path and the plan already lists "add maintenance batching for full hierarchy rebuilds if
corpus-scale runs require it" as remaining work, so it is acknowledged rather than overlooked.

## Verification

- Inspected the full diff and the surrounding CLI structure: the new `BackfillLegiHierarchy`
  subcommand, the `emit_ingest` match arm (`main.rs:614-619`), and `backfill_legi_hierarchy_payload`
  (`main.rs:1231-1246`). The payload follows the established sibling pattern
  (`require_existing_index_dir` → `open_index` → operation → `json!`), matching `embed_chunks_payload`
  exactly in shape and helper usage; field names (`hierarchy_backfilled_documents`,
  `hierarchy_backfill_invalidated_embeddings`) match the ingest command/manifest output, and
  `scope:"full"` cleanly distinguishes this from the in-line scoped backfill.
- Confirmed operator-safety properties: `open_index` → `ManagedPostgres::start_durable` acquires the
  data-dir advisory lock (so a concurrent ingest cannot race this command); the operation is a no-op
  on an empty/non-LEGI index (returns 0/0 early); and the readiness gate prevents serving stale
  search results post-invalidation.
- Confirmed the change is purely additive (new enum variant, match arm, payload fn, one import that
  was previously test-only) — no existing behavior altered, no regression surface.
- Reviewed the test assertions: enriched `hierarchy_path`, contextualized chunk body, cleared
  fingerprint, deleted `chunk_embeddings` row, and — importantly — that `ingest_run` has no
  `backfill-legi-hierarchy` row and `ingest_member` is empty, proving the command leaves run/member
  accounting untouched as the plan claims.
- Ran `cargo test -p jurisearch-cli --test cli_contract ingest_backfill_legi_hierarchy_updates_full_index`
  → **1 passed** (1.77s, against live Postgres).
- Ran `cargo clippy --workspace --all-targets -- -D warnings` → **clean**.
- Verified the plan update accurately replaces the prior "operator note" placeholder with a "Done"
  entry describing the new recovery command.

## Recommendations

1. Align the test's `graph_edges.payload` with the production `attributes:[{key,value}]` shape so the
   contract test exercises the same anchor-extraction path real edges take.
2. Add a second-invocation assertion to the test to pin command-level idempotency.
3. Surface an explicit "re-run `ingest embed-chunks`" next-step hint in the command output (or the
   operator runbook), since the command intentionally invalidates embeddings.
