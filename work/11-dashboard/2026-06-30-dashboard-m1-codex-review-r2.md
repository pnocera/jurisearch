# Codex Re-review - Dashboard M1 Shared Contracts

## Findings

No BLOCKER/WARN/NIT findings.

## Prior Findings

The nullable-coordinate blocker is resolved. `apps/dashboard/shared/src/dto.ts:86-102` now models `ingestJournals[].runId`, `ingestJournals[].journalCompactTimestamp`, `packageHighWaterMark.headSequence`, and `packageHighWaterMark.includedChangeSeqHigh` as nullable via `optStr`/`optNum`, matching the producer `Option<String>` / `Option<u64>` fields in `crates/jurisearch-producer/src/cursors.rs:38-54`. I re-checked the surrounding DTOs against `cursors.rs`, `runrecord.rs`, `status.rs`, and `manifest/remote.rs`; the remaining nullable producer fields in the dashboard's declared contract are nullable in TS, and the newly changed fields do not make producer-required coordinates disappear.

The added no-op fixture covers the previously missing shape. `apps/dashboard/fixtures/runrecord-legislation-noop-synthetic.json` is a valid finished `RunRecord` shape: `outcome="success"`, `exit_class="no-op"`, `ended_at` present, no `published_package`, one ingest coordinate with `run_id=null` / `journal_compact_timestamp=null`, and a `package_high_water_mark` object whose optional sequence coordinates are both `null`. That matches the producer path where `run_update` copies `ProducerCycleReport.head_sequence` / `included_change_seq_high` into `PackageHighWaterMark`, and those values are `(None, None)` when no catalog row exists. The test at `apps/dashboard/shared/src/dto.test.ts:83-98` asserts the nulls survive parsing rather than being rejected, dropped, or defaulted.

The bucket-drift warning is resolved. `apps/dashboard/shared/src/exit-class.drift.test.ts:56-153` now slices the real `pub fn exit_code_for` body, parses the `is_success(c) => 0` arm, the explicit string arms, and the `_ => 70` default, then compares both `exitCodeFor(cls)` and `EXIT_CLASS_TABLE[cls].exitCode` for every Rust-derived class. The sanity assertions at `:130-137` anchor the parser on the load-bearing buckets (`65`, `69`, `78`, `75`, and default `70`), so a parse miss or Rust re-bucket would fail rather than silently preserving the hardcoded TS expectation.

## Confirmed Good

- `severityOf` remains outcome-first in `apps/dashboard/shared/src/exit-class.ts:146-164`: running and unknown/null outcomes are neutral, success is ok, and only failure consults the producer exit-code bucket.
- The snake_case to camelCase mapping is still centralized through `object()` / `camelToSnake()` with explicit `from()` only for semantic renames (`manifestGeneratedAt` from `generated_at`).
- Unknown manifest/status fields remain forward-compatible because the object validator reads only declared fields.
- Running duration is still not fabricated in the DTO layer: `runDurationMs()` returns `null` unless both timestamps are present, matching `RunRecord::started` with `ended_at=None`.

## Validation

Ran from `apps/dashboard/`: `bun run test` -> 37 pass, 0 fail, 218 assertions across 7 files.

VERDICT: GO
