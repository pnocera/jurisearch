# Codex Review - Dashboard M1 Shared Contracts

## Findings

### BLOCKER: Run-record validator rejects producer-valid nullable coordinates

`apps/dashboard/shared/src/dto.ts:86-98` declares `ingestJournals[].runId`, `ingestJournals[].journalCompactTimestamp`, `packageHighWaterMark.headSequence`, and `packageHighWaterMark.includedChangeSeqHigh` as required non-null values. The Rust source of truth makes those fields nullable: `IngestJournalCoordinate.run_id: Option<String>`, `IngestJournalCoordinate.journal_compact_timestamp: Option<String>`, `PackageHighWaterMark.head_sequence: Option<u64>`, and `PackageHighWaterMark.included_change_seq_high: Option<u64>` in `crates/jurisearch-producer/src/cursors.rs:38-54`.

This means a valid persisted run record can fail dashboard parsing. The realistic case is an update that reaches ingest/publish but has no new archive cursor or no built package/no-op window: `update.rs` copies `report.journal_cursor` directly into `journal_compact_timestamp` at `crates/jurisearch-producer/src/update.rs:438-442`, and copies optional package sequence values into `PackageHighWaterMark` at `crates/jurisearch-producer/src/update.rs:366-370`. The current tests miss this because running records have empty arrays/null HWM, while the finished synthetic fixture uses only populated values.

Actionable fix: change those DTO fields to `optStr`/`optNum`:

- `runId: optStr`
- `journalCompactTimestamp: optStr`
- `headSequence: optNum`
- `includedChangeSeqHigh: optNum`

Then add a fixture or synthetic test with a finished/no-op-style run record where `ingest_journals[0].journal_compact_timestamp`, `package_high_water_mark.head_sequence`, and `package_high_water_mark.included_change_seq_high` are `null`, and assert the parsed values remain `null`.

### WARN: Exit-class drift test does not protect severity bucket drift for existing classes

`apps/dashboard/shared/src/exit-class.drift.test.ts:35-50` re-derives the Rust class vocabulary from `exit.rs` and `error.rs`, which is good for additions/removals/renames. It does not re-derive the `exit_code_for` mapping that drives `severityOf` for failures. The only bucket coverage is the hardcoded TypeScript test in `apps/dashboard/shared/src/exit-class.test.ts:30-43`, so if Rust changes an existing class from one sysexits bucket to another, the drift test stays green and the hardcoded TS expectation stays green.

For example, changing `integrity-failed` from `65` to `75` in `crates/jurisearch-producer/src/exit.rs:40-51` would change dashboard severity from `data` to `transient`, but the current drift guard would not catch it because the class set is unchanged.

Actionable fix: extend the drift test to parse `exit_code_for` from `exit.rs` and compare class-to-exit-code buckets against `exitCodeFor`/`EXIT_CLASS_TABLE`, including the default `70` bucket for classes from `ProducerError::class` that are not matched explicitly. Alternatively, expose a small producer-generated contract fixture for `{class, exit_code}` pairs and test the shared table against that.

## Confirmed Good

- The snake-case producer JSON to camel-case DTO conversion is centralized through `object()` and `camelToSnake()` in `apps/dashboard/shared/src/validate.ts:141-163` and `apps/dashboard/shared/src/mapping.ts:8-16`, with explicit `from()` overrides only where the source key is a semantic rename.
- Unknown fields are ignored by the object validator, so the manifest fixture's extra payload and active-baseline fields are forward-compatible rather than rejected.
- `severityOf` is outcome-first in `apps/dashboard/shared/src/exit-class.ts:146-164`: `running` and null/unknown outcomes stay neutral, success stays ok, and only failure consults `exitCodeFor`.
- Running duration is not synthesized in the DTO layer. `runDurationMs()` returns `null` unless both timestamps are present in `apps/dashboard/shared/src/mapping.ts:65-78`, matching `RunRecord::started` where `ended_at` is `None`.

VERDICT: FIXES_REQUIRED
