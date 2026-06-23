# Codex Review - Phase 2.5 Incremental Sync

Reviewed HEAD `830acee6f09f7cf5bf59559123146460be98f23c` against parent `f7f82301434115d916a7d137cd209c71e395283f`, focused on `crates/jurisearch-cli/src/main.rs` and `crates/jurisearch-cli/tests/cli_contract.rs` per `/tmp/codex-review-phase2-5.md`.

## BLOCKER

- `sync` can publish a completed freshness manifest for archives it did not process. `select_archives_to_process` correctly omits the baseline and filters deltas by `since_compact` (`crates/jurisearch-cli/src/main.rs:3722`), but both final manifests still derive `source_version`/`freshness.latest_archive` from `plan.deltas.last().unwrap_or(&plan.baseline)` (`crates/jurisearch-cli/src/main.rs:3655` and `crates/jurisearch-cli/src/main.rs:4059`). The final manifest is then saved even when the filtered `archives` list is empty (`crates/jurisearch-cli/src/main.rs:3928`, same pattern in the juri path), and `status` reads per-source freshness from the latest completed run manifest (`crates/jurisearch-storage/src/retrieval.rs:1039`). Repro shape: build a baseline, place one newer delta in `--archives-dir`, then run `sync --source cass --since 2999-01-01 --archives-dir ...`; no archive/member is visited, but the completed sync manifest reports the newest delta in the directory as the corpus freshness. That violates the requirement that status reports exact corpus freshness and that sync is honest about what it did. Fix by making manifest freshness/source_version for incremental sync come from the processed archive set, or by preserving the previous completed freshness when no archive is selected; add a contract test for a zero-selected/no-op sync and assert `status.corpus_sources` does not advance.

## WARN

- `normalize_since` accepts malformed values by stripping all non-digits before deciding validity (`crates/jurisearch-cli/src/main.rs:3743`). The documented/required inputs are `YYYY-MM-DD` or compact `YYYYMMDDHHMMSS`, but values like `2025/01/15`, `abc20250115xyz`, or `2025-01-15T00:00:00` normalize successfully instead of returning `bad_input`. This is not an off-by-one problem, and the lexicographic `>=` comparison is correct once both sides are 14 ASCII digits, but validation is looser than the contract says. Fix by matching the exact two accepted shapes before normalization, and add negative tests for separator/noise forms.

## NIT

- None.

## Verified Behavior

- The existing `ingest legi-archives` and `ingest juri-archives` dispatch sites pass `ArchiveSyncFilter::default()`, so the full ingest list remains baseline first followed by planner-ordered deltas.
- `sync` routes `legi` to the LEGI ingest path and `cass`/`capp`/`inca`/`jade` to the jurisprudence path via `ArchiveSource::from_token` plus `source.is_jurisprudence()`.
- Member-level compatibility blocking is still reached for selected archives before processing: both source paths call `ingest_resume_decision_with_client` with parser/schema/code/source payload compatibility, and `BlockedIncompatible` records a failed member rather than silently mixing incompatible payloads.
- The response reframing only replaces top-level `command`, `mode`, `source`, and `synced_since`; run status, counters, manifest, and replay snapshot cache remain the ingest path's values. The blocker above is specifically that the ingest path's manifest is not selection-aware under incremental filtering.
- I did not rerun the validation commands listed in the instruction file; this review is source/diff based.

VERDICT: FIXES_REQUIRED
