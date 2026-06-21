# Review — Ingest Accounting Storage APIs (commit f382819)

Verdict: FIXES_REQUIRED

Scope: commit `f382819` ("Add ingest accounting storage APIs") against
`work/03-implementation/IMPLEMENTATION_PLAN.md` Phase 1.0 "Ingest Operational
Accounting and Replay". This is the storage-accounting slice only (schema +
repository APIs + integration tests); CLI/status/quarantine/safe-mode wiring is
explicitly deferred.

---

## Blocking findings

### B1. `projection_coverage` total is inflated by the document→chunk fan-out (wrong metric)

`crates/jurisearch-storage/src/ingest_accounting.rs:465-475`

```sql
SELECT count(*)::bigint,
       count(DISTINCT d.document_id) FILTER (WHERE c.chunk_id IS NOT NULL)::bigint
FROM documents d
LEFT JOIN chunks c ON c.document_id = d.document_id;
```

`projection.get(0)` is assigned to `total_documents` (line 474), but `count(*)`
over `documents LEFT JOIN chunks` counts **joined rows**, not documents.
`chunks` is one-to-many on `document_id` (`UNIQUE (document_id, chunk_index)`,
`migrations.rs:57`), so a document with N chunks contributes N rows and a
chunkless document contributes 1 row. `count(*)` therefore equals
"sum of chunk counts + number of chunkless documents", not the document count.

Worked example — D1 (2 chunks), D2 (0 chunks):
- join rows: `(D1,c1), (D1,c2), (D2,NULL)` → `count(*) = 3`
- covered: distinct docs with a non-null chunk → `{D1}` = 1
- reported: `total=3, covered=1, percentage≈33%`
- correct: `total=2, covered=1, percentage=50%`

In realistic data (every real document is chunked into multiple chunks),
`projection_coverage.total` is dominated by the chunk count and
`projection_coverage.percentage` is driven far below 100% even when every
document is fully projected. This is a correctness defect in a function the
status doc lists as **Done** and that the Phase 1.0 acceptance criterion
("`status --json` can report latest ingest health, coverage, and recovery
warnings") depends on.

The `embedding_coverage` query immediately below (lines 477-487) is **correct**
because `chunk_embeddings.chunk_id` is a PRIMARY KEY (`migrations.rs:60-61`), so
`chunks LEFT JOIN chunk_embeddings` is 1:(0 or 1) and does not fan out. The
asymmetry is exactly why projection coverage is wrong and embedding coverage is
right.

Fix: make `total` a document count, e.g. `count(DISTINCT d.document_id)::bigint`
(keeping the FILTER-based covered column), or compute total from `documents`
directly (`SELECT count(*) FROM documents`) and covered via a separate
`EXISTS`/distinct query.

Test gap that masks it: the fixture
(`tests/ingest_accounting.rs:189-212`) inserts exactly **one** document with
**one** chunk, so `count(*) = 1` happens to equal the document count and
`assert_eq!(health.projection_coverage.total, 1)` (line 176) passes. A fixture
with one document and ≥2 chunks would fail that assertion and surface the bug;
please add that case alongside the fix.

---

## Non-blocking findings / suggestions

### N1. `compatibility_mismatch` reason is undifferentiated (payload-change vs code-change)

`ingest_accounting.rs:385-396`. The compatibility check ANDs all four fields and,
on any mismatch, returns `BlockedIncompatible` with `reason =
"compatibility_mismatch"` — evaluated **before** the status branch. This matches
the plan ("Block blind recovery when parser/schema/code/source-payload
compatibility differs; require targeted reprocess"), so it is plan-conformant.

However, the caller cannot distinguish two semantically different cases from the
returned `action`/`reason`:
- the **source payload changed** (content genuinely updated → the right response
  is usually reprocess), versus
- **parser/schema/code changed** (stale-row risk → the right response is a
  targeted migration/reprocess decision).

Both surface identically as `BlockedIncompatible` / `compatibility_mismatch`.
When the resume logic is wired into the ingest flow, downstream code will likely
need to know *which* field(s) diverged. Consider encoding the mismatched
dimension in `reason` (or a structured field on `IngestResumeDecision`) now,
while the API surface is new.

### N2. `replay_snapshot_status` is a hardcoded stub

`ingest_accounting.rs:520` always returns `"pending"`. The plan lists "replay
snapshot status" as a W2 metric; stubbing it is fine for this slice, but the
status doc's "Done" bullet implies the health metrics are complete without
noting this field is not yet computed. Minor doc-accuracy nit — consider listing
"replay snapshot status computation" under the Remaining bullet.

### N3. `ingest_member_payload_compat_idx` appears unused by any query in this slice

`migrations.rs` creates an index on
`(archive_name, member_path, source_payload_hash, parser_version,
schema_version, code_version)`, but `ingest_resume_decision` filters only on
`(archive_name, member_path)` and compares the version/hash fields in Rust, so it
uses `ingest_member_resume_idx`, not this one. The payload-compat index adds
write overhead for every member upsert without serving a current query. Confirm
it is for an imminent query, otherwise drop it until needed.

### N4. `attempt_count` is per-run, not cumulative across runs

`record_ingest_member` (lines 232-278) increments `attempt_count` only on
`ON CONFLICT (run_id, archive_name, member_path)`. A new run inserts a fresh row
with `attempt_count = 1`, so cross-run retries do not accumulate. If
"attempt_count" is meant to track total attempts for a member across resume runs
(plausible for retry/backoff policy), this will undercount. Confirm the intended
semantic; if cross-run accumulation is wanted it needs a different source
(e.g. seed from the prior latest row).

### N5. `load_ingest_health` is not a consistent snapshot

The function issues five independent queries on one connection without a
transaction (lines 419-487). Counts, error classes, and coverage can reflect
slightly different points in time under concurrent ingest. Acceptable for an
approximate health report; wrap in a `REPEATABLE READ` transaction if exactness
is ever required.

### N6. Additional test-coverage gaps (besides the multi-chunk case in B1)

`tests/ingest_accounting.rs` covers the happy paths well (compatible skip,
failed/unfinished retry, parser-version block, error aggregation, coverage). Not
covered:
- `BlockedIncompatible` triggered by `schema_version` / `code_version` /
  `source_payload_hash` divergence (only `parser_version` is exercised, line 79).
- `Discovered`-status resume → `previous_unfinished` (only `Parsed` is tested).
- Negative paths returning `IngestAccounting` errors: `finish_ingest_run` with a
  non-terminal status and with a nonexistent `run_id`; `update_ingest_member_status`
  on a nonexistent `member_id`.
- Run-level error (`record_ingest_error` with `member_id: None`) and the
  member `error_count` / `last_error_*` update path.
- `attempt_count` increment on re-recording the same member within a run.

---

## What is correct and matches the plan

- **Schema migration 3** (`migrations.rs`) adds `ingest_run`, `ingest_member`,
  `ingest_error` with run status, per-member archive/path/source/entity/date/
  status, structured error fields (`last_error_class/code/message`,
  `error_count`), and recovery-compatibility metadata (`parser_version`,
  `schema_version`, `code_version`, `source_payload_hash`) — matching the Phase
  1.0 task list. `CHECK` constraints on `status` columns match the enum string
  values exactly. `CURRENT_SCHEMA_VERSION` bumped to 3; `validate_migration_list`
  enforces contiguity and the version/latest match; the migration body and the
  `schema_migrations` bookkeeping are wrapped in `BEGIN; … COMMIT;`
  (`migrations.rs:225-231`), so migration 3 is applied atomically and is
  idempotent (`CREATE TABLE/INDEX IF NOT EXISTS`). `schema_migrations.rs` test
  updated to assert `3:ingest_operational_accounting`.

- **Resume decision** (`ingest_resume_decision`, lines 352-414) matches the
  required policy: skip only when `inserted`/`skipped` **and** all four
  compatibility fields match (`compatible_complete`); retry on `failed`
  (`previous_failed`) and on unfinished `discovered`/`parsed`
  (`previous_unfinished`); block on any compatibility divergence
  (`blocked_incompatible`). Lookup is cross-run (latest row by
  `updated_at DESC, member_id DESC`), correctly backed by
  `ingest_member_resume_idx`.

- **Structured error recording** (`record_ingest_error`, lines 305-350) inserts
  the error row and updates the member's `error_count` / `last_error_*` inside a
  single transaction; `member_id` is optional for run-level errors.

- **Idempotent member recording** uses `ON CONFLICT (run_id, archive_name,
  member_path)` matching the table's UNIQUE constraint, returning
  `member_id`/`attempt_count`/`status`.

- **Connection/error idioms** are consistent with the existing modules
  (`dense.rs`, `projection.rs`): one `postgres::Client::connect` per call,
  parameterized `$N` queries throughout (no SQL injection; JSON args via
  `COALESCE($N::text::jsonb, …)`), new `StorageError::IngestAccounting` variant
  added to `runtime.rs`.

- **Plan status accuracy**: the "Remaining for later 1.0 slices" bullet
  correctly scopes out LEGI ingest-flow wiring, quarantine file output,
  `status --json` exposure, safe-mode command flags, and query-gate blocking —
  the storage slice records `safe_mode` and supports quarantine traceability
  (`ingest_error.context`, member linkage) without implementing those consumers.
  The "Done" claims are accurate **except** that the projection-coverage metric
  (B1) is computed incorrectly.

---

## Verification notes

- I did not re-run the listed `cargo` commands; they were run before review and
  the integration tests require managed Postgres. Review was performed
  statically against the live diff and surrounding files.
- B1 is established from PostgreSQL aggregation semantics (`count(*)` over a
  one-to-many LEFT JOIN counts joined rows) plus the confirmed `documents`→
  `chunks` one-to-many cardinality (`UNIQUE (document_id, chunk_index)`,
  `migrations.rs:57`). The existing passing test does not contradict this: its
  single-document/single-chunk fixture makes `count(*)` coincide with the
  document count, so the green test masks the defect rather than refuting it.
- Migration atomicity, version bookkeeping, parameterization, and enum/CHECK
  alignment were verified by reading `migrations.rs`, `runtime.rs`, and
  `ingest_accounting.rs` directly.
- `git diff --check`, clippy, and the workspace test pass were reported clean by
  the pre-review verification; nothing in the static review contradicts that
  (B1 is a logic bug, not a lint/compile issue).
