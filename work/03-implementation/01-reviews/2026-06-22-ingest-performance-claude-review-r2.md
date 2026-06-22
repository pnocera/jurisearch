I've completed my inspection. All previously-flagged blockers and the HIGH finding are addressed, the new batching logic is correct (exhaustive counter merge, per-archive batching, idempotent resume), and the resume-backfill repair is sound (empty scope ⇒ full backfill, test-proven). Here is the review.

---

# Code Review (R2) — LEGI Ingest Performance / Resume Patch

Scope: re-review after `FIXES_REQUIRED`. Diff across `runtime.rs`, `ingest_accounting.rs`, `projection.rs`, `main.rs`, `cli_contract.rs`, `Cargo.toml`, `Cargo.lock`. Focus: did the fixes land correctly, and did the restructure (batched transactions + per-batch counter merge + index-stays-online) introduce new defects.

## 1. Findings (ordered by severity)

### Prior blockers — all resolved ✔

- **F1 (was BLOCKER) — resolved.** `runtime.rs:451-469`: the bulk profile now emits only `synchronous_commit='off'`, `wal_compression`, `max_wal_size`, checkpoint tuning, `shared_buffers`, `maintenance_work_mem`. `fsync='off'` and `full_page_writes='off'` are gone. A crash/`kill -9` during bulk ingest is now crash-*consistent* (WAL replay, no torn pages); at worst a small tail of `synchronous_commit=off` commits is lost and re-processed idempotently on resume. The headline guarantee ("restart without losing committed work") holds, and the silent-corruption tail is closed.
- **F2/F3/F4(index)/F6 — resolved.** The BM25 drop/recreate path is removed entirely (no `drop_deferred_legi_bulk_indexes` / `recreate_deferred_legi_bulk_indexes` / `DROP INDEX` / `chunks_bm25_idx` references remain in `main.rs`). The index stays online for the whole run, so an interrupted ingest no longer breaks lexical/hybrid search, there is no duplicated DDL to drift, no dead-connection rebuild to strand, and no `DROP INDEX` early-return between `start`/`finish`. `cli_contract.rs:1908,2003` assert `chunks_bm25_idx` exists both after the first run and after the resumed run.
- **F7 — resolved.** `runtime.rs:489-491`: comment now explains `ALTER SYSTEM RESET max_wal_size` is defensive cleanup of `postgresql.auto.conf`, while the bulk profile uses `jurisearch.conf`. `ALTER DATABASE … RESET synchronous_commit` (which *is* needed, since BulkIngest persists it via `ALTER DATABASE SET`) is retained.

### LOW — N1. Peak memory grows ~128× (new, from batching)
`main.rs:1371,1377` + `reader.rs:12,18`

`pending_members` buffers up to `LEGI_INGEST_TRANSACTION_BATCH_SIZE = 128` owned `ArchiveMember`s, each holding the full member payload (`bytes: Vec<u8>`, capped at `DEFAULT_MEMBER_BYTE_LIMIT = 16 MiB`). Worst-case resident set per archive jumps from ~16 MiB (one member) to ~2 GiB (128 × 16 MiB). LEGI article XML is typically a few KB, so realistic peak is modest, but the bound is real. Consider a byte-budget cap (flush when either count ≥ 128 *or* accumulated bytes ≥ N) rather than count-only. Non-blocking.

### LOW — N2. Quarantine orphans, now batch-wide (F5 persists + amplified)
`main.rs:1900-1918`, invoked via `record_legi_member_error` inside the batch transaction

Quarantine writes are non-transactional. If any member later in the same batch raises a fatal `StorageError`, the batch transaction rolls back the `ingest_error`/`ingest_member` rows for *every* quarantined member in that batch, but their files on disk remain — orphaned, with no DB record. The filename is deterministic per `(run_id, archive, member_path)`, so within a run it overwrites; but each resume uses a fresh `run_id` directory, so orphan dirs accumulate across interrupted runs. Operational noise, not a data-correctness issue.

### INFO — N3. Single long-lived `ingest_client` (F4 residual, impact reduced)
`main.rs:1329-1335`, reused through `:1482`

One client spans the whole multi-hour run; a transient drop fails every subsequent op and ends the run `Failed`. This is now low-impact: there is no final index rebuild to strand (F2 fix), so failure simply finalizes the run and resume continues. No `statement_timeout`/keepalive is set. Acceptable for a resumable job; note for operators.

### INFO — N4. Every compatible-skip resume pays a full unscoped backfill (F8 persists, by design)
`main.rs:1450-1456` → `projection.rs:318,348-352,406-411`

`full_resume_backfill = counters.skipped_compatible_members > 0` forces a `default()` (empty) scope, and an empty scope sets `$1::boolean = true`, making the candidate predicate `$1 OR …` match **all** legi articles. This is *correct* — it's exactly what repairs articles committed-but-not-yet-backfilled before a prior stop (test-proven), but a resume that processes only skips near the end of a large corpus still backfills the entire table. Surface this expectation.

### NIT — N5. Redundant length predicate in the flush
`main.rs:1378-1393`

`if len >= BATCH && let Err … {}` immediately followed by `if len >= BATCH { clear() }` double-evaluates the predicate. Folding into one `if len >= BATCH { process…?; clear(); }` block reads cleaner. No behavioral difference.

## 2. Open questions / residual risks

- **Stop procedure / orphaned postmaster.** Post-F1 there is no longer a corruption risk on Ctrl-C, but there is still no SIGINT handler, so Ctrl-C skips `Drop`/`pg_ctl -m fast stop` and orphans the postmaster holding the data-dir lock. Worth confirming `reclaim_data_dir`/`start_pg_ctl` cleanly reclaim a stale-but-running instance on the next invocation. Pre-existing, not introduced by this patch.
- **Async run-accounting tail.** `start`/`manifest`/`finish` now run on `ingest_client` under session `synchronous_commit=off`. A clean CLI exit checkpoints (fast stop) and makes them durable; a `kill -9` immediately after `finish` could leave the run `running` on recovery. Resume handles a stale `running` run, so this is acceptable, but it is untested.
- **Durability restore depends on a later durable start.** `ALTER DATABASE … SET synchronous_commit=off` persists in the catalog; it is reset by `apply_runtime_profile(Durable)` on the next `start_durable`. In the single-shot CLI model every non-ingest command restarts managed PG with the Durable profile, so durability is restored on the next command — fine. Only a deployment that *only ever* ran `ingest legi-archives` would keep it off.
- **No test** exercises crash-mid-batch resume, the async-tail-loss path, or the batch-rollback quarantine-orphan case. These are the hardest paths and remain uncovered.

## 3. Verification notes

- **`merge_committed` is exhaustive** (`main.rs:1232-1255`). Checked field-by-field against every `counters.*` mutation in the member path (`:1603,1604,1637,1666-1669,1701,1719,1737,1773,1774,1820-1822,1859,1877`): all 15 mutated fields are merged. The 3 unmerged fields — `visited_members` (incremented on the outer counter at `:1376`, never on `committed`), `hierarchy_backfilled_documents`, `hierarchy_backfill_invalidated_embeddings` (set post-loop at `:1459-1461`) — are correctly excluded. No silent count loss.
- **No dedup regression from per-batch reset.** `processed_article_document_ids` / `processed_section_source_uids` / `processed_text_source_uids` are write-only accumulators — read only at end for scope (`:1434-1448`) and manifest counts (`:1292-1294`), never via `.contains()` for a processing decision. Per-batch `committed` reset + `extend` merge yields a set identical to a single accumulator.
- **Per-archive batching is correct.** `pending_members` is declared *inside* the `'archives` loop (`:1371`), so every batch holds only the current archive's members and the `archive_name` label is always right; the guarded tail flush (`:1403-1416`, `fatal_error.is_none() && read_result.is_ok() && !pending_members.is_empty()`) drains the remainder before the next archive. Fatal/limit breaks `'archives`. Transaction errors propagate via `?` and roll back through `Transaction::drop`; resume re-processes idempotently (`ON CONFLICT DO UPDATE`).
- **Resume-backfill repair verified.** Empty scope ⇒ `full_scope=true` ⇒ full backfill; the regression test damages `documents.hierarchy_path` + `chunks.contextualized_body`, resumes via compatible-skip, and asserts `hierarchy_backfill_scoped_documents=0`, `hierarchy_backfilled_documents=1`, the document hierarchy and the chunk `contextualized_body`/`hierarchy_path` are repaired, and `chunks_bm25_idx` still exists (`cli_contract.rs:1944-2006`).
- **Durable restore path sound.** `jurisearch.conf` is regenerated without bulk GUCs each start (`runtime.rs:454-465`); `apply_runtime_profile(Durable)` resets `synchronous_commit` (`ALTER DATABASE … RESET`) + `max_wal_size` + `pg_reload_conf()`. Only `ingest legi-archives` uses `BulkIngest` via `open_index_for_bulk_ingest` (single caller, `main.rs:1327`); all other paths use `open_index`/`start_durable` (Durable).
- **`finish_ingest_run` terminal-status guard** is now present in both the standalone wrapper (`ingest_accounting.rs:243`) and `_with_client` (`:259`) — harmlessly redundant.
- **Cargo change** is the expected `postgres.workspace = true` added to `jurisearch-cli` for the directly-managed `postgres::Client`.
- I did not run the build; relying on the stated `cargo fmt --check`, `clippy --all-targets -D warnings`, `cli_contract`, and `cargo test --workspace` passes. The `C: GenericClient` threading and `&mut Transaction` plumbing are consistent with those results.

The blocker (F1) and the HIGH finding (F2) from the first review are fully resolved, and the supporting MEDIUM/LOW index issues fall out of removing the drop/recreate path. The batching restructure is correct: the counter merge is exhaustive, batches are archive-scoped, rollback is idempotent on resume, and the forced full backfill genuinely repairs the un-backfilled-articles case the design targets. Remaining items are LOW/INFO and non-blocking — chiefly the increased worst-case memory footprint (N1) and quarantine-orphan noise on rollback (N2), both worth a follow-up but neither a correctness blocker.

VERDICT: GO
