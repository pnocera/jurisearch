Confirmed: retrieval uses `contextualized_body @@@ query` and `paradedb.score()` (retrieval.rs:187,191,262,266) — both hard-require `chunks_bm25_idx`. Without it, lexical/hybrid retrieval **errors**, not merely slows. I have everything needed for the review.

---

# Code Review — LEGI Ingest Performance / Resume Patch

Scope: current working-tree diff across `runtime.rs`, `ingest_accounting.rs`, `projection.rs`, `main.rs`, `cli_contract.rs`, `Cargo.toml`, `Cargo.lock`. Goal: faster ingest, resumable without losing committed work, bulk settings applied "safely," final query readiness preserved.

## 1. Findings (ordered by severity)

### BLOCKER — F1. `fsync=off` + `full_page_writes=off` make the bulk profile unsafe against the exact failure the resume design targets
`crates/jurisearch-storage/src/runtime.rs:455-470` (bulk block in `write_runtime_conf`)

```
synchronous_commit = 'off'
fsync = 'off'
full_page_writes = 'off'
...
```

These three are not equivalent in risk:

- `synchronous_commit='off'` is **crash-safe**: on power loss / `kill -9` you lose at most the last fraction of a second of *uncommitted-to-disk* transactions, but the cluster stays consistent. This is the correct, safe lever and it delivers most of the write speedup. ✔
- `fsync='off'` and `full_page_writes='off'` are **not crash-safe**. Per Postgres semantics, an OS crash, power loss, or `kill -9` of the postmaster while these are off can leave the cluster **corrupt** — including torn pages from `full_page_writes='off'` on ext4/xfs (no atomic page-write guarantee). Corruption is not recoverable by resume; it can prevent startup entirely or, worse, silently return wrong rows.

This directly contradicts two stated intents: "stopped and restarted **without losing committed work**" and "apply bulk-load settings **safely** for ingest only." The ingest is multi-hour — precisely the window where a crash/OOM-kill is most likely, which is the reason resume exists. For a legal-search corpus, the silent-wrong-results mode (torn pages) is especially damaging.

Mitigating context: a *clean* stop path is safe. `ManagedPostgres::drop` → `stop()` runs `pg_ctl -m fast stop` (runtime.rs:241-258), and `jurisearch.conf` is regenerated on every start (runtime.rs:451-469), so a clean durable restart reverts these GUCs. But `Drop` only runs on a clean process exit; on Ctrl-C (no signal handler, no unwinding) `Drop` does not run and the postmaster is orphaned, and on `kill -9` / power loss the corruption window is open.

Recommendation: drop `fsync='off'` and `full_page_writes='off'` from the bulk profile. Keep `synchronous_commit='off'`, `wal_compression`, `max_wal_size`, checkpoint tuning, `shared_buffers`, `maintenance_work_mem` — all crash-safe and responsible for the bulk of the speedup. If `fsync='off'` is truly wanted, it must be gated behind an explicit "I accept cluster loss on crash" opt-in and never the default for a resumable job.

### HIGH — F2. An interrupted ingest leaves `chunks_bm25_idx` dropped → lexical/hybrid queries error until a run completes
`crates/jurisearch-cli/src/main.rs:1319-1324` (drop, called at :1396) and `:1326-1356` (recreate, called at :1504)

The index is dropped at run start and only recreated at the very end. Retrieval uses `contextualized_body @@@ q` and `paradedb.score()` (retrieval.rs:187,191,262,266), which **hard-require** the BM25 index — without it ParadeDB raises an error, so this is a query *failure*, not a slowdown.

Consequence: in the entire window between a stopped/failed ingest and the next *successful* completion, lexical and hybrid search are broken. Pre-patch, an interrupted ingest left a populated (if stale) index, so search still worked. This is a regression against "preserve final query readiness" for the stop-and-restart workflow the patch is built around — note that "stop and restart" implies the DB is queryable *while stopped*.

It also compounds with F4: if the fatal error is a dead/severed connection, the recreate on the same connection (:1504) also fails and the index stays absent on a Failed run.

Recommendation: recreate the index if missing at the *start* of a (resumed) run before processing, or rebuild it on clean stop, or at minimum document that queries require a completed ingest. A "create-if-absent at start, rebuild at end" pairing closes the window.

### MEDIUM — F3. BM25 index definition is duplicated and will silently drift from the schema
`crates/jurisearch-cli/src/main.rs:1332-1351` vs `crates/jurisearch-storage/src/migrations.rs:355-369`

The recreate DDL currently matches migration v9 **exactly** (verified char-for-char: `bm25 (chunk_id, contextualized_body)`, French stemmer/stopwords, `ascii_folding`). ✔ Correct today.

But the canonical definition now lives in two places. A future migration (v10) that alters the BM25 index would leave this CLI copy rebuilding the **stale v9** definition after every bulk ingest — silently reverting the schema. The new `cli_contract` assertion only checks the index *exists* (`count = 1`), not its definition, so the drift would pass CI.

Recommendation: source the DDL from one place (a shared `const` in `jurisearch-storage` reused by both the migration and the CLI), or add a test asserting the recreated index's `pg_get_indexdef` equals the migration's.

### MEDIUM — F4. Whole multi-hour run depends on a single long-lived `ingest_client`, including the final index rebuild
`crates/jurisearch-cli/src/main.rs:1362-1367` (connect + session `SET`), reused through `:1504`

Previously each helper opened a short-lived connection; now one connection spans the whole run and also performs the closing `recreate_deferred_legi_bulk_indexes`. A single drop (server checkpoint hiccup, OOM, idle/keepalive timeout) fails every subsequent op *and* the index rebuild, ending the run Failed with no BM25 index (see F2). No `statement_timeout`/keepalive tuning is set. The run is resumable, so this isn't data loss, but it widens the blast radius of one transient failure and worsens the "no index" window.

Recommendation: at minimum, perform the final index recreate on a fresh connection (or via the `&postgres` helper path used by the backfill at :1492) so a stale ingest connection can't strand the index in the dropped state.

### LOW — F5. Quarantine files are written inside the batch transaction; rollback orphans them
`crates/jurisearch-cli/src/main.rs:1907-1913` (`maybe_quarantine_payload` inside `record_legi_member_error`, invoked during batch processing)

Filesystem quarantine writes are non-transactional but now occur within the 128-member batch window. If a later member in the same batch triggers a fatal DB error, the transaction rolls back the `ingest_error`/`ingest_member` rows but the quarantine file remains, and the member is re-quarantined on resume. Result: orphan/duplicate quarantine artifacts with no matching DB record. Operational noise, not a data-correctness issue.

### LOW — F6. `DROP INDEX` failure strands the run in `running`
`crates/jurisearch-cli/src/main.rs:1396`

`drop_deferred_legi_bulk_indexes(...).map_err(storage_error_object)?;` early-returns *after* `start_ingest_run` has recorded the run as `running` (:1379) but *before* `finish_ingest_run`. A failed `DROP INDEX IF EXISTS` (rare) leaves an orphaned `running` row. This is a new early-return point between start and finish introduced by the patch. Consider capturing it into `fatal_error` instead of `?` so the run is finalized as `Failed`.

### LOW / INFO — F7. `ALTER SYSTEM RESET max_wal_size` resets a GUC the bulk profile never set via `ALTER SYSTEM`
`crates/jurisearch-storage/src/runtime.rs:495-510`

`max_wal_size` is applied through `jurisearch.conf` (regenerated each start), not `ALTER SYSTEM`. The durable-path `ALTER SYSTEM RESET max_wal_size` therefore targets `postgresql.auto.conf`, which this patch never writes — so it's a harmless no-op against current code (defensive cleanup of older/manual tuning at most). Worth a one-line comment to prevent future confusion, or drop it. (The `ALTER DATABASE ... RESET synchronous_commit` reset, by contrast, *is* necessary because the bulk path persists it via `ALTER DATABASE SET` at :490-494.)

### INFO — F8. Every resume with a compatible-skip pays a full hierarchy backfill + full BM25 rebuild
`crates/jurisearch-cli/src/main.rs:1485-1506`

`full_resume_backfill = counters.skipped_compatible_members > 0` forces an *unscoped* backfill, and the index is rebuilt over all chunks regardless. This is correct (and the regression test proves it repairs un-backfilled committed articles), but it means a resume that processes only skips near the end of a large corpus still does full-table backfill + full index rebuild. "Resume" is correct but not cheap; surface this expectation to operators.

## 2. Open questions / residual risks

- **How is the long ingest actually stopped?** If always a clean `pg_ctl -m fast stop` (i.e., the process exits normally so `Drop` runs), F1's corruption risk is largely theoretical. If operators Ctrl-C the CLI (orphaning the postmaster, `Drop` skipped) or `kill -9`, F1 is live. There is no SIGINT handler to convert Ctrl-C into a clean stop — worth confirming the intended stop procedure.
- **Verified merge completeness:** every counter field mutated in the member path (skips, failures, inserts, metadata roots, processed-id sets, quarantines — main.rs:1641-1915) is propagated by `merge_committed` (:1231-1255); only `visited_members` (tracked on the outer counter) and the post-loop `hierarchy_*` fields are intentionally excluded. No silent count loss. ✔
- **Batch-granularity resume:** committed work is now per-128-member batch instead of per-member. A crash mid-batch re-processes ≤127 members on resume. Safe because inserts are `ON CONFLICT DO UPDATE` idempotent and `ingest_member` rows commit with the batch. Acceptable, but the lost-work granularity grew from 1 to 128 — intentional trade, worth noting.
- **No test exercises the crash-mid-batch resume, the interrupted "no index" window (F2), or fsync behavior** — these are hard to test but are the riskiest paths.

## 3. Verification notes

- Batch flush control flow (main.rs:1404-1465) is correct: full batches flush-then-clear; the post-loop tail flush is guarded by `fatal_error.is_none() && read_result.is_ok() && !pending_members.is_empty()`; fatal/limit breaks out of `'archives`. No double-processing or dropped tail.
- Recreate runs unconditionally (:1504) even after `fatal_error`, so a logic-level failure on a live connection still restores the index — good (the gap is only the dead-connection case, F4).
- CLI recreate DDL matches migration v9 byte-for-byte (verified). The new test damages hierarchy, resumes via compatible-skip, and asserts full backfill repairs both `documents` and `chunks` plus index existence — solid coverage for the resume-backfill path.
- Durable recovery is sound for the *clean* path: `jurisearch.conf` regenerated per start drops bulk GUCs; `ALTER DATABASE ... RESET synchronous_commit` + `pg_reload_conf()` restore durability on the next normal command (which any query issues via `open_index`/`start_durable`).
- I did not run the build; relying on the stated `fmt`/`clippy -D warnings`/`test --workspace` passes. The generic `C: GenericClient` threading and `&mut Transaction` passing are consistent with those results.

The patch is well-structured and the resume/backfill correctness is sound. The blocker is purely the durability/safety of the bulk GUC choice: `fsync='off'` + `full_page_writes='off'` undermine the headline guarantee ("restart without losing committed work") under the crash scenario the feature exists to handle, with a silent-corruption tail. Removing those two lines (keeping `synchronous_commit='off'` and the rest) resolves it at near-zero cost to performance. F2 (dropped-index query window) is the next most important and should be addressed or explicitly documented.

VERDICT: FIXES_REQUIRED
