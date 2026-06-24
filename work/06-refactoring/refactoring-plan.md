# Jurisearch Refactoring Plan

Date: 2026-06-24

## Executive Summary

The immediate refactoring target should be `crates/jurisearch-cli/src/main.rs`. It is 11,716 lines and 462 KB in the current working tree, which is larger than the next production file by roughly 4.6x. It currently owns command parsing, command dispatch, retrieval payloads, ingest payloads, eval runners, release-gate validation, embedding/runtime setup, JSONL serving, output formatting, and a large private unit-test module.

The recommended strategy is a behavior-preserving module split inside the existing `jurisearch-cli` binary crate. Do not introduce a new library crate yet. The current code uses many CLI-private argument types and helper functions; keeping the first split within the binary crate avoids public API churn while still reducing review and merge risk.

After the CLI split, address the medium-large parser/projection/retrieval files with narrower internal module splits. Those files are not as urgent as `main.rs`; their public surfaces are already more coherent.

## Current Size Profile

Measured with `.venv` and `target` pruned:

| File | Lines | Notes |
| --- | ---: | --- |
| `crates/jurisearch-cli/src/main.rs` | 11,716 | Primary refactor target. One binary module contains almost every CLI concern. |
| `crates/jurisearch-cli/tests/cli_contract.rs` | 4,954 | Large integration-contract suite; worth splitting after the CLI module split. |
| `crates/jurisearch-ingest/src/legi/mod.rs` | 2,535 | LEGI domain types, XML parsing, canonicalization, links, chunks, date validation. |
| `crates/jurisearch-storage/src/retrieval.rs` | 1,330 | Retrieval query types and multiple JSON SQL emitters. |
| `crates/jurisearch-ingest/src/juri/mod.rs` | 1,326 | Jurisprudence parsing, chunks, inferred citations, publisher links. |
| `crates/jurisearch-storage/src/projection.rs` | 1,308 | LEGI/decision projection, metadata roots, hierarchy backfill, chunk embeddings. |
| `crates/jurisearch-storage/src/ingest_accounting.rs` | 1,128 | Ingest run accounting, resume, health, replay snapshots, readiness cache. |
| `crates/jurisearch-official-api/src/lib.rs` | 1,101 | PISTE config/client/retry/token/error helpers in one lib file. |
| `crates/jurisearch-core/src/schema.rs` | 1,063 | Mostly compiled schema payload generation. |
| `crates/jurisearch-embed/src/lib.rs` | 1,057 | Config, fingerprinting, OpenAI-compatible client, tokenizer/truncation, errors. |

Working tree note: `crates/jurisearch-cli/src/main.rs` and `crates/jurisearch-storage/src/migrations.rs` already had local modifications when this plan was written. Treat this plan as based on the current working tree, not pristine `HEAD`.

## Observed CLI Structure

High-signal symbols and line bands in `main.rs`:

- `Cli`, `Command`, and most `Args`/session arg structs are concentrated near lines 141-930.
- `run` is the top-level dispatcher at lines 1041-1151 and matches 22 top-level commands.
- `emit_eval` is at lines 1227-1291 and delegates to eval payload builders; it has a small artifact-writing duplication for `FranceLegi` while other eval commands use `emit_artifact`.
- Retrieval payloads include `search_payload`, `search_with_postgres`, `compare_payload`, `fetch_payload`, `cite_payload`, `context_payload`, and `related_payload`.
- Ingest dispatch is `emit_ingest` at lines 4209-4334, with `ingest_legi_archives_payload`, `ingest_juri_archives_payload`, `embed_chunks_payload`, and backfill helpers later in the file.
- JSONL serving/session dispatch lives around lines 7446-7624.
- Setup/status/doctor/inspection/gates are concentrated from about line 7626 onward, including `status_payload`, `phase1_gate_payload`, `phase2_gate_payload`, benchmark artifact validators, ingest-health JSON shaping, and helper functions.
- Private unit tests start at line 10123 and cover phase gates, Judilibre zones, run IDs, archive date normalization, citation parsing, help coverage, session contract parity, and benchmark artifact validators.

The current shape is not just "large"; it also makes unrelated changes conflict. For example, adding a CLI flag, changing an eval gate, and touching ingest archive behavior all edit the same `main.rs`.

## Primary Goal

Reduce `crates/jurisearch-cli/src/main.rs` to a small binary entrypoint and stable module map:

```text
crates/jurisearch-cli/src/
  main.rs
  args.rs
  dispatch.rs
  output.rs
  retrieval.rs
  eval.rs
  ingest.rs
  session.rs
  serve.rs
  status.rs
  embedding_runtime.rs
  index_runtime.rs
  gates/
    mod.rs
    phase1.rs
    phase2.rs
```

The exact names can change during implementation, but the dependency direction should stay simple:

```text
main -> dispatch
dispatch -> args + command modules + output
command modules -> shared runtime/config/output helpers
status/gates -> embedding_runtime + index_runtime + storage/core
session -> command payload functions, not shell-style emitters
```

Do not make `jurisearch-cli` a library crate just to satisfy tests in the first pass. Keep private unit tests next to the moved code with `#[cfg(test)]` modules. A public CLI library can be considered later if downstream reuse appears.

## Phase 0: Baseline and Guard Rails

1. Capture current behavior before moving code:
   - `cargo fmt --check`
   - `cargo test -p jurisearch-cli`
   - `cargo test -p jurisearch-core`
   - `cargo test -p jurisearch-ingest`
   - `cargo test -p jurisearch-storage`
   - `cargo test -p jurisearch-embed`
   - `cargo test -p jurisearch-official-api`
2. If the full storage tests require local PostgreSQL/runtime assets and are slow, keep `cargo test -p jurisearch-cli` as the required gate for every CLI extraction commit and run the broader storage/ingest suites at phase boundaries.
3. Preserve current command JSON exactly. This repo's status/eval gates are contract-heavy, so refactoring should not change payload keys, ordering assumptions in tests, exit codes, or session parity.
4. Keep each commit mechanical enough to review by moved-symbol diff. Avoid semantic improvements in the same commit as a large move.

## Phase 1: Mechanical CLI Shell Split

Target: create a small `main.rs` without moving command behavior yet.

Proposed changes:

1. Move parser and enum definitions from the top of `main.rs` into `args.rs`:
   - `Cli`
   - `Command`
   - `SearchArgs`, `FetchArgs`, `CiteArgs`, `RelatedArgs`, `ContextArgs`, `CompareArgs`
   - `StatusArgs`, `InspectArgs`, `VersionsArgs`, `DiffArgs`
   - `JsonlArgs`, `ServeArgs`, `SyncArgs`
   - `ModelCommand`, `EvalCommand`, `EvalSubcommand`, `IngestCommand`, `IngestSubcommand`
   - CLI value enums such as `CliKind`, `CliSearchMode`, `CliOutputFormat`, `CliGroupBy`, archive/source enums
   - session arg structs only if they remain tightly coupled to clap defaults; otherwise move them with `session.rs` in Phase 2.
2. Move output helpers into `output.rs`:
   - `write_json`
   - `write_session_response`
   - `emit_error`
   - `emit_artifact`
3. Move `run` into `dispatch.rs`.
4. Leave payload functions in `main.rs` during this phase if needed; import them from `dispatch.rs` as `crate::search_payload`, etc. This creates a compilable intermediate step before deeper extraction.
5. Replace the special-case artifact write in `emit_eval` for France-LEGI with `emit_artifact(response, out_path)` if tests confirm byte-identical behavior.

Expected result:

- `main.rs` should mostly declare modules, call `dispatch::run()`, map errors to exit code, and keep any still-unmoved implementation.
- This phase is mostly `pub(crate)` visibility changes and imports.

Validation:

- `cargo fmt --check`
- `cargo test -p jurisearch-cli`

## Phase 2: Session and Serving Split

Target: isolate the JSONL protocol from one-shot command dispatch.

Move to `session.rs`:

- `run_jsonl`
- `dispatch_session_request`
- session-specific deserialization structs
- `session_*_payload` wrappers

Move to `serve.rs`:

- `run_serve`
- TCP bind validation
- Unix socket stale-socket handling
- calls into `session::serve_jsonl` or keeps a small common helper there

Keep the session module calling payload builders directly, not CLI emitters. That preserves the current architecture where one-shot commands write/exit and session commands return `Result<Value, ErrorObject>`.

Validation:

- `cargo test -p jurisearch-cli session_dispatch_matches_one_shot_only_set`
- `cargo test -p jurisearch-cli every_command_and_arg_has_help`
- Full `cargo test -p jurisearch-cli`

## Phase 3: Retrieval Command Split

Target: move high-frequency query commands out of the CLI monolith.

Move to `retrieval.rs`:

- `search_payload`
- `search_with_postgres`
- `compare_payload`
- `fetch_payload`
- `cite_payload`
- `context_payload`
- `related_payload`
- `expand_payload`
- command emitters: `emit_search`, `emit_fetch`, `emit_cite`, `emit_context`, `emit_related`, `emit_compare`, `emit_expand`
- retrieval-specific validation helpers such as cursor/date/candidate helpers if only used here

Keep shared index-opening and query-readiness helpers in `index_runtime.rs` rather than letting retrieval own them.

Possible follow-up after the move:

- Consider moving pure argument-to-query conversion methods (`SearchArgs::retrieval_options`, `SearchArgs::decision_filters`) with `SearchArgs` in `args.rs`; retrieval can consume the methods without knowing clap details.

Validation:

- `cargo test -p jurisearch-cli`
- Any available focused CLI contract tests for search/fetch/cite/context/related/compare.

## Phase 4: Status, Runtime, and Gate Split

Target: isolate release-gate truthfulness and runtime readiness from command plumbing.

Move to `embedding_runtime.rs`:

- `embedding_config_from_env`
- `loaded_embedding_config`
- model-cache helpers
- endpoint status/probe helpers
- embedding pool config/worker helpers
- `ensure_embedding_runtime_ready`

Move to `index_runtime.rs`:

- `open_index`
- `open_index_for_bulk_ingest`
- `require_existing_index_dir`
- `require_configured_index_dir`
- `configured_index_dir`
- `ensure_query_readiness`
- storage/embedding error mapping if it is shared across command modules

Move to `status.rs`:

- `setup_payload`
- `model_fetch_payload`
- `status_payload`
- `doctor_payload`
- `stats_payload`
- `inspect_payload`
- `versions_payload`
- `diff_payload`
- `status_index_and_ingest_health`
- `ingest_health_payload`

Move to `gates/phase1.rs` and `gates/phase2.rs`:

- `phase1_gate_payload`
- `phase1_external_benchmark_payload*`
- `phase1_france_legi_payload*`
- `phase1_*_artifact_errors`
- `phase2_gate_payload`
- `phase2_benchmark_payload*`
- `phase2_*_artifact_errors`
- gate-specific tests currently in the large `#[cfg(test)] mod tests`

Reasoning:

The gates are high-risk contractual code. They should not stay mixed with socket serving, model cache probing, archive ingestion, and command argument parsing. Splitting them also makes future reviews easier because gate changes will be localized.

Validation:

- `cargo test -p jurisearch-cli phase1`
- `cargo test -p jurisearch-cli phase2`
- `cargo test -p jurisearch-cli france_legi`
- Full `cargo test -p jurisearch-cli`

## Phase 5: Ingest Command Split

Target: move official-source ingestion orchestration out of the CLI entrypoint while keeping storage and parser APIs unchanged.

Move to `ingest.rs`:

- `emit_ingest`
- `sync_payload` and `emit_sync`/dispatch equivalent if it remains a stub or source-ingest helper
- archive manifest helpers:
  - `legi_archive_manifest`
  - `juri_archive_manifest`
  - `planned_archive_manifest`
  - `select_archives_to_process`
- `ingest_legi_archives_payload`
- `ingest_juri_archives_payload`
- quarantine helpers
- `embed_chunks_payload`
- `backfill_legi_hierarchy_payload`
- ingest run-id/date/source filter helpers

Dependency caution:

This module will need both parser crates and storage projection/accounting imports. Keep it in the CLI crate first because it is orchestration: it converts CLI flags to parser/storage calls and JSON artifacts. Do not move it into `jurisearch-ingest` unless a non-CLI caller needs the same orchestration API.

Validation:

- `cargo test -p jurisearch-cli`
- `cargo test -p jurisearch-ingest`
- `cargo test -p jurisearch-storage ingest_accounting`
- Archive subset tests if local fixtures are available.

## Phase 6: Split CLI Contract Tests

Target: reduce `crates/jurisearch-cli/tests/cli_contract.rs` from 4,954 lines into domain-focused integration suites.

Suggested layout:

```text
crates/jurisearch-cli/tests/
  cli_help_contract.rs
  cli_session_contract.rs
  cli_retrieval_contract.rs
  cli_status_contract.rs
  cli_eval_contract.rs
  cli_ingest_contract.rs
  support/
    mod.rs
    fixtures.rs
    assertions.rs
```

Rules:

- Move shared command-running helpers into `tests/support`.
- Keep test names stable when practical so historical failure searches still work.
- Do this after source modules are split; otherwise the test refactor and source refactor will conflict in reviews.

Validation:

- `cargo test -p jurisearch-cli --tests`

## Secondary Refactors

These should wait until the CLI is no longer a merge hotspot.

### `crates/jurisearch-ingest/src/legi/mod.rs`

Current public surface includes parsed LEGI domain types, canonical documents/chunks/edges, parser entrypoints, and `source_payload_hash`. Natural split:

```text
legi/
  mod.rs
  types.rs
  parser.rs
  canonical.rs
  chunks.rs
  links.rs
  xml.rs
  dates.rs
```

Keep `mod.rs` re-exporting the current public names so callers continue using `jurisearch_ingest::legi::{...}`. Move tests carefully because parser behavior is data-sensitive.

### `crates/jurisearch-ingest/src/juri/mod.rs`

Natural split:

```text
juri/
  mod.rs
  types.rs
  parser.rs
  chunks.rs
  inferred_citations.rs
  publisher_links.rs
  xml.rs
  dates.rs
```

This mirrors the LEGI parser split and separates XML streaming from canonical chunk/edge generation.

### `crates/jurisearch-storage/src/retrieval.rs`

Natural split:

```text
retrieval/
  mod.rs
  types.rs
  hybrid.rs
  citation.rs
  fetch.rs
  context.rs
  stats.rs
  versions.rs
  related.rs
  sql.rs
```

Keep `mod.rs` re-exporting existing public functions (`hybrid_candidates_json`, `fetch_documents_json`, `resolve_legi_citation_json`, etc.) until callers are migrated. The SQL builders should be split by response family, not by generic helper style.

### `crates/jurisearch-storage/src/projection.rs`

Natural split:

```text
projection/
  mod.rs
  legi.rs
  decisions.rs
  metadata.rs
  hierarchy_backfill.rs
  embeddings.rs
  graph_edges.rs
```

This file has several independent write paths. The split should preserve prepared statement reuse (`LegiProjectionStatements`, document projection statements) and keep transaction ownership unchanged.

### `crates/jurisearch-storage/src/ingest_accounting.rs`

Natural split:

```text
ingest_accounting/
  mod.rs
  runs.rs
  members.rs
  errors.rs
  resume.rs
  health.rs
  readiness.rs
  replay_snapshot.rs
```

This is lower urgency because the existing file is linear and cohesive, but replay snapshot/readiness code is a good later extraction target.

### `crates/jurisearch-official-api/src/lib.rs`

Natural split:

```text
config.rs
client.rs
auth.rs
retry.rs
error.rs
```

This should be straightforward because the public API is already centered on `OfficialApiConfig`, `PisteClient`, `RetryPolicy`, and `OfficialApiError`.

### `crates/jurisearch-embed/src/lib.rs`

Natural split:

```text
config.rs
fingerprint.rs
client.rs
tokenizer.rs
error.rs
```

Do this after CLI extraction because `main.rs` currently has substantial embedding runtime code that may clarify which responsibilities belong in `jurisearch-embed` versus the CLI.

### `crates/jurisearch-core/src/schema.rs`

This file is large but likely generated/compiled-contract style. Prefer splitting only if it is hand-maintained and frequently edited. A better long-term move may be schema fragments plus a deterministic assembly test.

## Visibility and API Policy

- Prefer `pub(crate)` for new CLI modules.
- Preserve existing public crate exports for `jurisearch-ingest`, `jurisearch-storage`, `jurisearch-embed`, and `jurisearch-official-api`.
- Avoid moving CLI orchestration into library crates unless a real non-CLI caller exists.
- Avoid introducing traits for command handlers in the first pass. Straight modules and functions are enough.
- Keep `ErrorObject` boundaries intact. Commands currently return `Result<Value, ErrorObject>` internally and emit JSON/exit at the outer layer; that is a good boundary to preserve.

## Suggested Commit Plan

1. `cli: split args and output helpers from main`
2. `cli: move dispatcher into dispatch module`
3. `cli: move jsonl session and serve protocol modules`
4. `cli: move retrieval command payloads`
5. `cli: move status/runtime helpers`
6. `cli: move phase gates and artifact validators`
7. `cli: move ingest command orchestration`
8. `cli: split integration contract tests`
9. `ingest: split legi parser internals`
10. `ingest: split juri parser internals`
11. `storage: split retrieval SQL emitters`
12. `storage: split projection write paths`

Each source-split commit should compile independently and should avoid semantic changes except the small `emit_artifact` duplication cleanup if done with a focused test.

## Risk Areas

- Gate truthfulness: Phase 1/2 status gates validate artifacts, floors, evidence, and routing claims. Move tests with the exact gate functions.
- Session parity: session commands are intended to match one-shot command payloads where supported. Keep session wrappers close to payload builders or add explicit tests when moving.
- Artifact output: `eval france-legi`, `eval france-juris`, `eval run`, and `eval tune` all print and optionally write JSON artifacts. Any output-helper consolidation must preserve newline and pretty-print behavior.
- Ingest accounting: ingest commands coordinate storage run lifecycle, manifest updates, member status, quarantine, and query-readiness invalidation. Avoid moving storage transaction ownership across crate boundaries in the first refactor.
- Dirty working tree: rebase or commit existing local edits before a large move, otherwise mechanical moves will be harder to review.

## Definition of Done

The refactor is done when:

- `crates/jurisearch-cli/src/main.rs` is under roughly 250 lines and contains only module declarations, `main`, and trivial wiring.
- No single production file remains above roughly 2,000 lines unless intentionally generated or contract data.
- Existing CLI JSON contracts and exit behavior are unchanged.
- `cargo fmt --check` passes.
- `cargo test -p jurisearch-cli` passes after every CLI-phase commit.
- Broader crate tests pass at phase boundaries.
- CodeGraph can show command-specific context without returning most of the CLI file for unrelated tasks.
