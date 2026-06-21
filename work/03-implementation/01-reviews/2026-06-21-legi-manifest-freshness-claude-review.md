I have completed my review. The working tree matches commit `f1384cf` exactly, so I reviewed both the live tree and the commit.

# Claude Review - LEGI Ingest Manifest Freshness

Verdict: GO

The slice is correct, well-tested, and safely completes the 1.1 coverage/freshness manifest work. The manifest round-trips through storage (`start_ingest_run` → `update_ingest_run_manifest` → `load_ingest_health`), is exposed in both the command JSON and `status.ingest_health.latest_manifest`, and is built deterministically. The added field is additive and backward-compatible. Key timing behavior is sound: the final manifest is persisted **before** `finish_ingest_run` and runs unconditionally, so a `failed` run still records its partial coverage — and the CLI contract test proves this (run_status `"failed"`, yet `status` reports `coverage.persisted_metadata_members == 3`, which only the *final* manifest carries, confirming the initial all-zero manifest was overwritten).

Verified specifics:
- `latest_archive = plan.deltas.last().unwrap_or(&plan.baseline)` is correct — deltas are sorted ascending and the planner only admits deltas strictly newer than the baseline (`planner.rs:119`), so `.last()` is the newest archive.
- `source_version`/`latest_archive_timestamp` use `to_string()` → `"20250101-000000"`; `*_compact` uses `compact()` → `"20250101000000"`. Test assertions match.
- `manifest jsonb NOT NULL DEFAULT '{}'` and `updated_at` exist (`migrations.rs:118-132`); `update_ingest_run_manifest` SQL (`$2::text::jsonb`, `updated == 1` guard) is correct and mirrors `finish_ingest_run`.
- Determinism: `parsed_metadata_roots`/`unsupported_roots` are `BTreeMap`; `deltas`/`skipped` are sorted in the planner; `serde_json` map keys are sorted (no `preserve_order`). Stable output.
- No uncommitted changes to `crates/`; `cargo test --workspace` + clippy already green.

## Non-blocking suggestions

1. **Error precedence on double-failure** — `crates/jurisearch-cli/src/main.rs:780-783`. If a `fatal_error` is already set (member-processing or backfill error) and `update_ingest_run_manifest` *also* fails, the manifest-write error overwrites the root cause — losing it from both the returned `ErrorObject` and the persisted `ingest_run.error_message`. The manifest UPDATE still happens, so only the diagnostic is degraded. Suggest preserving the first error:
   ```rust
   if let Err(error) = update_ingest_run_manifest(&postgres, run_id.as_str(), &final_manifest_json) {
       fatal_error.get_or_insert_with(|| storage_error_object(error));
   }
   ```

2. **Theoretical dangling run on serialization** — `crates/jurisearch-cli/src/main.rs:778-779`. `serde_json::to_string(&final_manifest)?` adds a `?` early-return *between* `start_ingest_run` and `finish_ingest_run`; if it errored the run would be stuck in `running`. Serializing an in-memory `Value` is effectively infallible, so this is only theoretical — but `let final_manifest_json = final_manifest.to_string();` (infallible) removes the path and is simpler.

3. **Freshness vs. actual ingestion semantics** — `freshness.latest_archive`/`source_version` describe the newest archive in the *plan*, not what was actually ingested. On a `--limit-members`-truncated or failed run, freshness reads optimistically while only `coverage`/`run_status` reflect real work. Since `load_ingest_health` selects `latest_manifest` by `started_at DESC` **regardless of status**, a downstream "are we fresh to X?" check reading only `latest_manifest.freshness` could be misled by a failed/partial run. The manifest itself does not embed run status. Recommend either embedding a `run_status`/completeness marker in the manifest, or documenting that freshness must be gated on `ingest_health.latest_run_status == "completed"` / `latest_completed_run_id` and cross-checked against `coverage`.

4. **Minor consistency notes (no action required):** (a) top-level command counters are now duplicated inside `manifest.coverage.*` — both derive from the same `counters`, so they stay consistent; (b) jsonb storage reorders object keys, so the manifest's key order differs between command output and `status` output — field-based consumers are unaffected, but a byte/hash comparison across the two surfaces would not match; (c) `pending_ingest_health()` omits `latest_manifest`, consistent with its minimal-placeholder design (consumers branch on `state`).

## Verification commands

Inspected (already run locally) and recommended to re-run:
- `cargo test -p jurisearch-storage --test ingest_accounting` — confirms `latest_manifest` round-trip (fixture `{"fixture":true}` → `health.latest_manifest["fixture"] == true`).
- `cargo test -p jurisearch-cli ingest_legi_archives_records_accounting_and_quarantines_failures --test cli_contract` — confirms command `manifest` fields and `status.ingest_health.latest_manifest` coverage/freshness on a failed run.
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `git diff --check`
