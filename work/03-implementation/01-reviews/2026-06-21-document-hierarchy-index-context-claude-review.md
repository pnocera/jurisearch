# Claude Review — Phase 1.2 Indexed Context Sibling Lookup (migration 7)

- Step reviewed: uncommitted document-`hierarchy_path` materialization + index for `context --siblings`
- Reviewer: Claude (Opus 4.8), 2026-06-21

This step materializes `documents.hierarchy_path` (migration 7), indexes `(source, kind,
hierarchy_path)`, keeps the column synchronized on insert/backfill, and rewrites
`context_documents_json` to join siblings on the indexed column instead of the prior per-candidate
chunk lateral full scan. It directly closes the prior review's #1 concern (full-scan/unbounded
sibling lookup) and satisfies the plan item. Correctness is sound, migrations are atomic, tests are
well-extended, and all checks pass. Non-blocking suggestions only.

## What I verified (correct)

- **Migration 7 is atomic and backfills consistently.** Each migration runs inside `BEGIN; … COMMIT;`
  with its `schema_migrations` insert (`migrations.rs run_migrations`), so the `ALTER` + full-table
  `UPDATE` + `CREATE INDEX` either fully apply or roll back — no partial column population that could
  make siblings asymmetric. The backfill prefers `canonical_json->'hierarchy_path'`, falls back to the
  first non-empty chunk path, else keeps `'[]'`. For LEGI data `document.hierarchy_path ==
  canonical->'hierarchy_path' == chunk.hierarchy_path` at every write, so the result is unambiguous.
- **Write paths keep the column in sync.** `insert_legi_documents` writes `hierarchy_path` (column 14,
  `$14::jsonb`) from `document.hierarchy_path` with a matching `ON CONFLICT DO UPDATE`
  (`projection.rs:67-86,141-159`; param binding verified, no off-by-one), and the hierarchy backfill
  sets `hierarchy_path = COALESCE($2::jsonb->'hierarchy_path', hierarchy_path)` alongside the
  `canonical_json` update (`projection.rs:440-442`). So every mutator that touches the document path
  also updates the indexed column.
- **The sibling join is now an index-backed equality seek.** `JOIN documents d ON d.source=t.source
  AND d.kind=t.kind AND d.hierarchy_path=t.hierarchy_path AND d.document_id<>t.document_id`
  (`retrieval.rs:345-355`) matches the leading columns of `documents_context_hierarchy_idx (source,
  kind, hierarchy_path)`, replacing the removed per-candidate `chunks` lateral. jsonb has a btree `=`
  operator class, so the composite seek is valid. The 55-sibling generated fixture in
  `retrieval_smoke` exercises this returning many siblings (plus the cap), and the empty-path pair
  exercises the `array_length(t.hierarchy_path) > 0` suppression.
- **Replay snapshot correctly extended.** `load_replay_snapshot` adds `hierarchy_path::text` to the
  document `row_hash` (`ingest_accounting.rs:683-684`). This is a freshly computed health-report
  fingerprint (used only to populate `IngestHealthReport`, not compared against a stored prior hash),
  so it is a correct extension that lets replay-divergence detection see the new column.
- **Tests cover the new path well.** `schema_migrations` asserts migration 7, the index, and the
  column exist, and — importantly — `chunk_provenance_backfill_sql_materializes_existing_canonical_json`
  now also runs the document `hierarchy_path` backfill SQL and asserts the column is populated from
  canonical JSON (the migration-backfill path is no longer untested). Fixtures across `retrieval_smoke`,
  `cli_contract`, and `structural_survival` populate the column consistently. All pass locally; clippy
  clean.
- **Plan accuracy:** the Done line correctly describes migration 7 + index + sync + indexed sibling
  lookup and removes the corresponding Remaining item.

## Non-blocking suggestions

1. **(Top) Btree-on-jsonb size limit is a latent hard-failure risk.** `documents_context_hierarchy_idx`
   indexes the full `hierarchy_path` jsonb. A btree index tuple is capped (~2704 bytes); a pathological
   deep/long path could exceed it and make the document `INSERT` (or the migration's index build)
   **fail outright**, not just run slow. Current LEGI paths (≈5–6 levels, confirmed by the passing
   5-level `structural_survival` insert) are well under the limit, so this won't trigger today — but
   consider indexing a digest instead (`md5(hierarchy_path::text)` expression index, or a stored
   `hierarchy_key` column) to remove the ceiling entirely, or at least note the assumption. This is the
   one item that could cause a hard ingestion error rather than a perf regression.
2. **Target vs. sibling derivation is asymmetric.** The target's `hierarchy_path` is derived
   (`stored_hierarchy_path` if non-empty, else `canonical_json->'hierarchy_path'`, else `[]`) while the
   sibling join uses the raw `d.hierarchy_path` column (`retrieval.rs:315-327` vs `345-355`). If the
   column ever drifted from canonical for some rows, a target could display a path yet match zero
   siblings. All write paths + migration keep them in sync, so it's currently safe — but making the
   target also key off the raw column (or dropping the now-redundant canonical fallback) would make the
   two sides provably consistent.
3. **Three slightly different derivation rules.** The migration backfill (canonical-first,
   `jsonb_typeof='array'` so an empty array short-circuits), the query target (column-first,
   `array_length>0`), and the old chunk-first behavior differ in precedence/empty-handling. Identical
   for LEGI data, but consolidating to one documented rule would reduce future drift risk.
4. **Add a migration-7 operator note.** Migration 6 got a note about its one-time full-table rewrite;
   migration 7 also does a full `documents` UPDATE plus a `CREATE INDEX`, which on a corpus-scale index
   is a one-time, write-locking startup cost. A parallel note keeps the runbook complete.
5. **Confirm the index is actually chosen.** The index is structured for the seek, but the only
   sibling-scale test (55 rows) is too small for the planner to prefer it. Consistent with this
   project's evidence practice, an `EXPLAIN (ANALYZE)` of `context --siblings` on a loaded real code
   would confirm the index scan (vs. a seq scan) materializes the intended corpus-scale win.

## Verification performed

- Read the full diffs for `migrations.rs`, `projection.rs`, `retrieval.rs`, `ingest_accounting.rs`,
  and the four test files; confirmed migration atomicity (`run_migrations` `BEGIN/COMMIT`), the
  insert/backfill column sync, the index/query column alignment, the replay-snapshot usage (fresh
  fingerprint, not a stored comparison), and the insert param binding.
- Ran `cargo test -p jurisearch-storage --test schema_migrations --test retrieval_smoke --test
  structural_survival` (5 passed) and `cargo clippy -p jurisearch-storage -p jurisearch-cli
  --all-targets -- -D warnings` (clean). The author's broader `cargo test --workspace` is consistent
  with these results.

Verdict: GO
