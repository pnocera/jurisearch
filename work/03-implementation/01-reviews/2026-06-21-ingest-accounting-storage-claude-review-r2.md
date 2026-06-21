# Re-review — Ingest Accounting Storage APIs (HEAD `7cb54be`, after `f382819`)

Verdict: GO

Scope: re-review of HEAD after `f382819` ("Add ingest accounting storage APIs")
and `7cb54be` ("Fix ingest accounting coverage metric") against
`work/03-implementation/IMPLEMENTATION_PLAN.md` Phase 1.0 "Ingest Operational
Accounting and Replay". This is the storage-accounting slice only (schema +
repository APIs + integration tests); CLI/status/quarantine/safe-mode wiring is
explicitly deferred. The prior review
(`2026-06-21-ingest-accounting-storage-claude-review.md`) returned
FIXES_REQUIRED for a single blocking item (B1).

---

## Blocking findings

None. The one blocking item from review 1 is resolved.

### B1 (RESOLVED) — `projection_coverage` now counts distinct documents

`crates/jurisearch-storage/src/ingest_accounting.rs:465-475`

```sql
SELECT count(DISTINCT d.document_id)::bigint,
       count(DISTINCT d.document_id) FILTER (WHERE c.chunk_id IS NOT NULL)::bigint
FROM documents d
LEFT JOIN chunks c ON c.document_id = d.document_id;
```

The fix changes the `total` column from `count(*)` to
`count(DISTINCT d.document_id)` (`ingest_accounting.rs:467`), so the
document→chunk fan-out of the `LEFT JOIN` no longer inflates the total. `total`
is now a true document count and `covered` (distinct documents with at least one
chunk) remains a subset of it, so `covered ≤ total` holds and the percentage is
bounded to a sensible `[0, 100]`. This is exactly the fix the prior review
recommended. The asymmetric `embedding_coverage` query (`ingest_accounting.rs:477-487`)
was already correct (`chunk_embeddings.chunk_id` is a PRIMARY KEY,
`migrations.rs:60-61`, so `chunks LEFT JOIN chunk_embeddings` is 1:(0 or 1)) and
is unchanged.

**Now genuinely tested.** The fixture (`tests/ingest_accounting.rs:189-222`)
was strengthened to two documents:
- D1 `legi:LEGIARTI000006419320@…` with **2** chunks (`chunk:1240:0`,
  `chunk:1240:1`), both embedded;
- D2 `legi:LEGIARTI000000000001@…` deliberately **chunkless**.

The join therefore produces 3 rows `((D1,c0),(D1,c1),(D2,NULL))`. The assertion
`health.projection_coverage.total == 2` (`tests/ingest_accounting.rs:176`) is now
a real regression guard: it passes under the fixed `count(DISTINCT …)` (=2) and
would fail under the old `count(*)` (=3). `covered == 1` (only D1 has chunks) is
also asserted (line 175). Embedding assertions were updated consistently to
`covered == 2 / total == 2` (lines 177-178). The D2 insert is schema-valid: no
constraint forces a document to own chunks, and all `NOT NULL` document columns
are supplied (`migrations.rs:27-44`).

I confirmed this directly rather than relying on the reported runs — see
Verification notes; the integration test compiles and passes in a managed
Postgres, and the second `vector_literal(1)` format argument matches the added
embedding placeholder (no arg-count mismatch).

---

## Other scoped areas — re-checked, no blocking issues

`7cb54be` touched only the projection query, the test fixture, the plan status
line, and the (now-committed) prior review doc; `f382819`'s schema, repository
APIs, resume logic, and error recording are byte-for-byte unchanged. I
re-confirmed each area the prior review cleared:

- **Schema migration 3** (`migrations.rs`): `ingest_run` / `ingest_member` /
  `ingest_error` tables, `CHECK` constraints matching the enum strings,
  `CURRENT_SCHEMA_VERSION` bumped to 3, contiguity validation, and atomic
  `BEGIN; … COMMIT;` application with idempotent `IF NOT EXISTS`. The
  `schema_migrations` integration test (asserting `3:ingest_operational_accounting`)
  passes here.
- **Repository APIs**: idempotent member recording via
  `ON CONFLICT (run_id, archive_name, member_path)`; parameterized `$N` queries
  throughout; `StorageError::IngestAccounting` variant.
- **Resume decisions** (`ingest_resume_decision`): skip only on
  `inserted`/`skipped` with all four compatibility fields matching; retry on
  `failed`/unfinished; block on any compatibility divergence; cross-run latest
  lookup backed by `ingest_member_resume_idx`.
- **Structured errors** (`record_ingest_error`): error-row insert plus member
  `error_count`/`last_error_*` update in one transaction; optional `member_id`
  for run-level errors.
- **Health metrics** (`load_ingest_health`): member counts and error-class
  aggregation scoped to the latest run; projection/embedding coverage now
  correct; recovery warnings for non-completed runs and failed members.
- **Plan status accuracy**: the "Remaining for later 1.0 slices" bullet
  (`IMPLEMENTATION_PLAN.md:495`) now also lists "compute replay snapshot status
  beyond the current `pending` storage placeholder", which addresses the prior
  N2 doc-accuracy nit. The remaining "Done" claims are accurate.

---

## Non-blocking suggestions (carried forward — still open, none blocking)

These were raised in review 1, are not regressions, and remain reasonable to
defer. Listed for tracking when the resume/health logic is wired into the ingest
flow.

- **N1.** `compatibility_mismatch` is undifferentiated — caller cannot tell a
  source-payload change (→ reprocess) from a parser/schema/code change (→
  targeted migration) from `action`/`reason` alone
  (`ingest_accounting.rs` resume branch). Consider encoding the mismatched
  dimension on `IngestResumeDecision` while the API surface is still new.
- **N2 (partially addressed).** `replay_snapshot_status` is still the hardcoded
  `"pending"` stub (`ingest_accounting.rs:520`); the plan now flags it under
  Remaining, so the doc/code mismatch is resolved. Compute it when replay
  snapshots land.
- **N3.** `ingest_member_payload_compat_idx` (`migrations.rs`) is not used by any
  query in this slice (resume filters on `(archive_name, member_path)` and
  compares versions in Rust via `ingest_member_resume_idx`); it adds per-upsert
  write overhead. Confirm an imminent query needs it, else drop until then.
- **N4.** `attempt_count` is per-run (incremented only on the
  `(run_id, archive_name, member_path)` conflict), so cross-run retries restart
  at 1. Confirm the intended semantic before retry/backoff policy depends on it.
- **N5.** `load_ingest_health` runs five independent queries on one connection
  without a wrapping transaction, so the snapshot is not point-in-time under
  concurrent ingest. Acceptable for an approximate health report.
- **N6.** Test-coverage gaps remain: `BlockedIncompatible` from
  `schema_version`/`code_version`/`source_payload_hash` divergence (only
  `parser_version` is exercised); `Discovered`→`previous_unfinished`; negative
  paths (`finish_ingest_run`/`update_ingest_member_status` on bad ids); run-level
  error (`member_id: None`); and the within-run `attempt_count` increment.
- **N7 (new, minor observation, not blocking).** Projection and embedding
  coverage are corpus-wide (no `run_id` scoping), unlike the member/error counts
  which are scoped to the latest run via `$1`. This is a defensible design for a
  "coverage" metric (it answers "what fraction of the whole corpus is
  projected/embedded", not "of this run"), but worth stating explicitly when
  `status --json` surfaces both groups so the two scopes aren't read as one.

---

## Verification notes

Unlike review 1, I ran the relevant checks directly in this environment (managed
Postgres was available):

- `cargo test -p jurisearch-storage --test ingest_accounting --no-run` — compiles
  clean (confirms the added `vector_literal(1)` argument matches placeholders).
- `cargo test -p jurisearch-storage --test ingest_accounting` —
  `1 passed; 0 failed`. This exercises the fixed `count(DISTINCT …)` query against
  the new D1(2 chunks)+D2(0 chunks) fixture, so the green result confirms the
  fix and that the test is a real guard (it would report `total = 3` and fail
  under the reverted `count(*)`).
- `cargo test -p jurisearch-storage --test schema_migrations` — `1 passed`
  (migration-3 assertion).
- `cargo clippy -p jurisearch-storage --all-targets -- -D warnings` — clean.
- `git diff --check d1c08d4..HEAD` — clean (no whitespace/conflict markers).
- Cardinality assumptions re-confirmed from `migrations.rs`:
  `chunks UNIQUE (document_id, chunk_index)` (one-to-many, line 57) drives the
  fan-out the fix corrects; `chunk_embeddings.chunk_id PRIMARY KEY` (line 61)
  keeps embedding coverage exact.
- The full workspace test/clippy pass was reported by the pre-review
  verification; nothing in this static + targeted re-run contradicts it.
