# Jurisearch Refactoring Plan

Date: 2026-06-24
Revised: 2026-06-25 (resynced to working tree after zone/enrichment landings)

## Executive Summary

The immediate refactoring target should be `crates/jurisearch-cli/src/main.rs`. It is now 13,747 lines and ~553 KB in the current working tree, which is larger than the next production file (`jurisearch-ingest/src/legi/mod.rs`, 2,535 lines) by roughly 5.4x. It currently owns command parsing, command dispatch, retrieval payloads, zone retrieval, decision-part/zone/legislation-citation enrichment, ingest payloads, eval runners, release-gate validation, embedding/runtime setup, JSONL serving, output formatting, and a large private unit-test module.

> **Resync note (2026-06-25):** No refactoring from this plan has started yet — `main.rs` is still a single module. Since the plan was first written (2026-06-24) the file grew by ~2,000 lines because a new **zone / decision-part / legislation-citation enrichment** domain landed: `zone_search_payload` and `ensure_zone_retrieval_readiness`; decision-part fetch (`DecisionPart`, `annotate_fetched_parts`, Judilibre zone helpers); the `eval france-juris-zones` benchmark; and five new `ingest` subcommands (`enrich-zones`, `build-zone-units`, `embed-zone-units`, `collect-legislation-citations`, `enrich-legislation-citations`). The phases below now account for that domain. All line-band references were re-measured against the current tree.

The recommended strategy is a behavior-preserving module split inside the existing `jurisearch-cli` binary crate. Do not introduce a new library crate yet. The current code uses many CLI-private argument types and helper functions; keeping the first split within the binary crate avoids public API churn while still reducing review and merge risk.

After the CLI split, address the medium-large parser/projection/retrieval files with narrower internal module splits. Those files are not as urgent as `main.rs`; their public surfaces are already more coherent. Note that `jurisearch-official-api/src/lib.rs` has since grown to 1,418 lines (PISTE + Legifrance citation enrichment surface) and is now the most urgent of the secondary targets.

## Current Size Profile

Measured with `.venv` and `target` pruned:

| File | Lines | Notes |
| --- | ---: | --- |
| `crates/jurisearch-cli/src/main.rs` | 13,747 | Primary refactor target. One binary module contains almost every CLI concern (+2,031 since 2026-06-24). |
| `crates/jurisearch-cli/tests/cli_contract.rs` | 5,049 | Large integration-contract suite; worth splitting after the CLI module split. |
| `crates/jurisearch-ingest/src/legi/mod.rs` | 2,535 | LEGI domain types, XML parsing, canonicalization, links, chunks, date validation. |
| `crates/jurisearch-official-api/src/lib.rs` | 1,418 | PISTE + Legifrance config/client/retry/token/error helpers in one lib file (+317 since 2026-06-24; now the top secondary target). |
| `crates/jurisearch-storage/src/retrieval.rs` | 1,392 | Retrieval query types and multiple JSON SQL emitters. |
| `crates/jurisearch-ingest/src/juri/mod.rs` | 1,326 | Jurisprudence parsing, chunks, inferred citations, publisher links. |
| `crates/jurisearch-storage/src/projection.rs` | 1,308 | LEGI/decision projection, metadata roots, hierarchy backfill, chunk embeddings. |
| `crates/jurisearch-storage/src/ingest_accounting.rs` | 1,128 | Ingest run accounting, resume, health, replay snapshots, readiness cache. |
| `crates/jurisearch-core/src/schema.rs` | 1,077 | Mostly compiled schema payload generation. |
| `crates/jurisearch-embed/src/lib.rs` | 1,057 | Config, fingerprinting, OpenAI-compatible client, tokenizer/truncation, errors. |
| `crates/jurisearch-storage/src/runtime.rs` | 836 | Managed-Postgres runtime/lifecycle (added 2026-06-21; below the original top-10 cutoff). Below the urgency bar; listed for awareness. |
| `crates/jurisearch-storage/src/zone_units.rs` | 652 | Zone-unit derivation/storage (added 2026-06-24). Supports the new zone retrieval index. |

Working tree note: apart from this plan document itself (currently an uncommitted edit), the source tree is clean as of 2026-06-25. The local `main.rs` and `migrations.rs` modifications noted in the original draft have since been committed, so once this plan edit is committed the refactor can start from a clean `HEAD`. Zone-related storage files (`zone_units.rs`, `zone_retrieval.rs`, `france_juris.rs` all added 2026-06-24; `runtime.rs` from 2026-06-21) are all under the ~2,000-line urgency bar.

## Observed CLI Structure

High-signal symbols and line bands in `main.rs` (re-measured 2026-06-25):

- `Cli`, `Command`, and most `Args`/session arg structs are concentrated near lines 178-1208. This band now also holds the zone-retrieval enums (`CliZone` ~355, `CliEnrichZoneOrder` ~375), the `EvalFranceJurisZonesArgs` struct (~884), and the expanded `IngestSubcommand` (~943-1079) with its five new enrichment variants.
- `run` is the top-level dispatcher at lines 1224-1340 and matches the same set of ~22 top-level commands.
- `emit_eval` is at lines 1410-1483 and delegates to eval payload builders; it has a small artifact-writing duplication for `FranceLegi` while other eval commands use `emit_artifact`. The eval block now also contains the zone benchmark path (`eval_france_juris_zones_payload` ~2834, `france_juris_zone_retrieval_category` ~2900, `zone_benchmark_category` ~2995, `zone_benchmark_artifact` ~3019).
- Retrieval payloads include `search_payload` (~3282), `search_with_postgres` (~3659), `compare_payload` (~3899), `fetch_payload` (~4056), `cite_payload` (~5180), `context_payload` (~5266), and `related_payload` (~3857). The new zone retrieval path lives alongside them: `ensure_zone_retrieval_readiness` (~3331) and `zone_search_payload` (~3373).
- A new **decision-part / zone / legislation-citation enrichment** band sits between fetch and the session wrappers (~4089-5178): `DecisionPart`/`annotate_fetched_parts`/`official_decision_part`/`zone_cache_action` (decision-part fetch), `parse_visa_citation`/`collect_legislation_citations_payload`/`enrich_legislation_citations_payload`/`legifrance_code_search_body` (legislation citations), and `enrich_decision_from_judilibre*`/`normalize_judilibre_zones`/`zone_text_hash`/`cache_zone_status_with_client` (Judilibre zone enrichment). This band is the bulk of the +2,000 line growth and is a strong candidate for its own module(s).
- Ingest dispatch is `emit_ingest` at lines 5455-5676, with `ingest_legi_archives_payload` (~5843), `ingest_juri_archives_payload` (~6209), `embed_chunks_payload` (~7718), and the new zone-unit pipeline: `enrich_zones_payload` (~7297), `build_zone_units_payload` (~7500), `embed_zone_units_payload` (~7578), plus `backfill_legi_hierarchy_payload` (~7207).
- Embedding pool/runtime config lives ~7886-9180 (`embedding_endpoint_pool_configs`, `embed_and_insert_*_with_pool`, `loaded_embedding_config`, `model_cache_status`, endpoint status helpers).
- JSONL serving/session dispatch lives around lines 9232-9460 (`serve_jsonl`, `run_serve`, `run_jsonl`, `dispatch_session_request`, and the `session_*_payload` wrappers, several of which are interleaved earlier near ~5299-5440 and ~9435-9460).
- Setup/status/doctor/inspection/gates are concentrated from about line 9461 onward, including `status_payload` (~9564), `doctor_payload` (~9621), `phase1_gate_payload` (~9896), `phase2_gate_payload` (~10612), benchmark artifact validators, `zone_retrieval_status_block` (~10996), `status_index_and_ingest_health` (~11011), `ingest_health_payload` (~11112), and helper functions.
- Private unit tests start at line 11986 and cover phase gates, Judilibre zones, run IDs, archive date normalization, citation parsing, help coverage, session contract parity, and benchmark artifact validators.

The current shape is not just "large"; it also makes unrelated changes conflict. For example, adding a CLI flag, changing an eval gate, touching ingest archive behavior, and enriching Judilibre zones all edit the same `main.rs`. The new enrichment domain has made this worse: zone retrieval, decision-part fetch, and legislation-citation collection are now interleaved with the pre-existing retrieval and ingest code.

## Primary Goal

Reduce `crates/jurisearch-cli/src/main.rs` to a small binary entrypoint and stable module map:

```text
crates/jurisearch-cli/src/
  main.rs
  args.rs
  command_registry.rs   # single inventory of name/session-availability/schema-names/handler (see SOLID/DRY follow-ups)
  dispatch.rs
  output.rs              # serialization/emission only: write_json, emit_error, session response
  request.rs            # shared command request structs (SearchRequest, FetchRequest, …) + TryFrom<*Args>
  retrieval/            # split per-command so it does not become a second monolith (SRP)
    mod.rs              # re-exports narrow payload fns
    search.rs           # search_payload
    zone.rs             # zone_search_payload, ensure_zone_retrieval_readiness
    fetch.rs            # fetch_payload (calls enrichment::annotate_fetched_parts)
    cite.rs             # cite_payload, citation state, apply_online_citation_confirmation
    context.rs          # context_payload (structure reader)
    related.rs          # related_payload (graph reader)
    compare.rs          # compare_payload
    expand.rs           # expand_payload
  citation.rs           # shared pure citation parser (parse_citation_target etc.) used by retrieval AND eval
  ascii.rs              # shared case-insensitive ascii find helpers (find_ascii_ci, rfind_ascii_ci) used by retrieval AND enrichment
  date.rs               # shared date/calendar helpers (is_valid_iso_date, days_in_month, is_leap_year, today_utc, unix_seconds, civil_from_days) used by retrieval/status/eval
  errors.rs             # shared ErrorObject constructors + storage/embed error mapping used by ALL command modules (definitive owner)
  query_support.rs      # shared retrieval-query helpers (parade_query_text, validate_retrieval_options) used by retrieval AND eval
  legifrance_search.rs  # shared Legifrance request-body builder (sanitize_legifrance_query, legifrance_code_search_body) used by retrieval (cite --online) AND enrichment/legislation
  enrichment/           # NEW domain (decision-part / zone / legislation citations)
    mod.rs
    archive.rs          # shared official_api_responses archive: archive_exchange, sha256_hex (+ archive_local_unsupported)
    decision_part.rs    # DecisionPart, annotate_fetched_parts, official_decision_part
    judilibre_zones.rs  # enrich_decision_from_judilibre*, normalize_judilibre_zones, cache helpers
    legislation.rs      # parse_visa_citation, collect/enrich_legislation_citations_payload
  eval/                 # split per benchmark family (SRP) instead of one flat eval.rs
    mod.rs
    generic.rs          # eval_run_payload, eval_tune_payload, eval_phase1_payload
    france_legi.rs      # France-LEGI official benchmark
    france_juris.rs     # France-juris benchmark
    zones.rs            # advisory france-juris-zones zone benchmark
    artifact.rs         # shared metric/category/artifact helpers (only genuinely shared bits)
  ingest.rs             # archives + embed-chunks + enrich-zones/build-zone-units/embed-zone-units
                        #   (introduces an internal ArchiveIngestRun runner — see SOLID/DRY follow-ups)
  session.rs
  serve.rs
  status.rs
  embedding_runtime.rs
  index_runtime.rs
  gates/
    mod.rs
    support.rs          # shared artifact load/parse/diagnostics + dotted-pointer + validator-result shaping
    phase1.rs
    phase2.rs
```

The exact names can change during implementation, but the dependency direction should stay simple:

```text
main -> dispatch
dispatch -> args + command modules + output
command modules -> shared leaf helpers (citation/ascii/date/errors/query_support) + runtime/config/output
enrichment -> enrichment/archive + official-api client + storage (decision_zones / legislation tables) + index_runtime
status/gates -> embedding_runtime + index_runtime + storage/core
session -> command payload functions, not shell-style emitters
```

### Shared leaf-helper modules (cross-cutting)

**Principle (the recurring failure mode this plan must avoid):** any pure or low-level helper used by **two or more** command modules goes into its own small leaf module that all callers import — it is never left behind in `main.rs`, and sibling command modules never import each other just to reach it. The command modules (`retrieval`, `eval`, `status`, `ingest`, `enrichment/*`) depend on these leaves; the leaves depend on nothing in the CLI but `args`/std/crates. The following leaves are required because the move otherwise breaks compilation or forces an avoidable sibling dependency (all verified against source 2026-06-25):

- `citation.rs` — `parse_citation_target` + `ParsedCitationTarget` + their private parser closure (see Phase 3a). Callers: `cite_payload` (retrieval) and `france_juris_cite_documents` (eval).
- `ascii.rs` — `find_ascii_ci` (`main.rs:3622`), `rfind_ascii_ci` (`main.rs:3636`). Callers: `legi_citation_routing` (retrieval) and `heuristic_dispositif` (`enrichment/decision_part`).
- `date.rs` — the **calendar** validators `is_valid_iso_date` (`main.rs:11814`), `days_in_month` (`main.rs:11832`), `is_leap_year` (`main.rs:11842`) (distinct from the cheap `is_iso_date` at `main.rs:3608` that stays in `retrieval.rs` with `legi_citation_routing`); plus the current-date helpers `today_utc` (`main.rs:11925`) with its private dependencies `unix_seconds` (`main.rs:11918`) and `civil_from_days` (`main.rs:11931`). Callers: calendar validators via `validate_as_of` → `cite_payload`/`context_payload` (retrieval) and `diff_payload` (status, `main.rs:9828`); `today_utc` is passed as `unwrap_or_else(today_utc)` across eval (`main.rs:1843`, `2264`, `2962`) and retrieval (`main.rs:3414`, `3682`, `3910`, `5183`). `validate_as_of` itself (`main.rs:11269`) stays in `retrieval.rs` and imports `date.rs`. (General rule for every leaf move: a listed helper moves **with its private helper closure** unless a dependency is separately assigned elsewhere.)
- `query_support.rs` — `parade_query_text` (`main.rs:11879`) and `validate_retrieval_options` (`main.rs:420`). Callers span eval (`eval_run_payload`, France-LEGI/juris/zones search) and retrieval (`search_payload`, `zone_search_payload`, `compare_payload`). (Alternatively make these `pub(crate)` in `retrieval.rs` and let `eval.rs` import them; a dedicated leaf is cleaner and avoids eval→retrieval coupling.)
- `legifrance_search.rs` — the Legifrance **request-body** builder `legifrance_code_search_body` (`main.rs:4591`) + its dependency `sanitize_legifrance_query` (`main.rs:4569`) and the `LEGIFRANCE_QUERY_MAX_CHARS` const (`main.rs:4561`), with their tests. Cross-command: `enrich_legislation_citations_payload` (`main.rs:4776`, enrichment) **and** `cite_payload`'s `--online` path via `apply_online_citation_confirmation` → `legifrance_code_search_body` (`main.rs:11716`, retrieval). This is the CLI-side JSON shaping; it stays distinct from the official-api crate's generic `legifrance_search_exchange`. (`legifrance_response_has_results` is currently used only by `enrich_legislation_citations_payload` — `main.rs:4792`, passed as `.is_some_and(legifrance_response_has_results)` — so it stays with `enrichment/legislation.rs`.)
- `errors.rs` — the `ErrorObject` constructors and error mapping used everywhere: `index_unavailable` (`main.rs:11232`), `dependency_unavailable` (`main.rs:11243`), `no_results` (`main.rs:11253`), `upstream_unavailable` (`main.rs:11261`), `index_not_query_ready`, `storage_error_object` (`main.rs:11846`), `embedding_error_object`/`embedding_error_object_with_context` (`main.rs:11856`). These appear across eval, retrieval, status, ingest, enrichment, and embedding paths, so they need a single shared home rather than living in any one command module. **Decision: `errors.rs` is the definitive owner** — it owns error *construction/mapping*, while `output.rs` owns only *serialization/emission* (`write_json`, `emit_error`, session-response writing) and depends on `ErrorObject`. Do not leave this to the mechanical move.
- `embedding_runtime.rs` — `PreparedQueryEmbedder` (`main.rs:3528`) moves here (not `retrieval.rs`): it is built by both eval search paths (`france_legi_search_documents`, `france_juris*` categories) and retrieval (`search_with_postgres`, `compare_payload`), so it belongs with the other embedding runtime that both import.
- `enrichment/archive.rs` — `archive_exchange`, `sha256_hex`, `archive_local_unsupported` (see Phase 3b). Callers: `enrichment/legislation.rs` and `enrichment/judilibre_zones.rs`.

These leaves should be extracted in (or before) the first phase that moves one of their callers, so no command-module move ever has to reach back into `main.rs`. In practice: pull the leaves out as a small early step in Phase 3a, since retrieval is the first command module to move and it touches most of them.

The `enrichment/` subtree is new relative to the original plan. It collects the decision-part fetch path, Judilibre zone caching, and Legifrance legislation-citation resolution that landed since 2026-06-24. The unifying rationale is **official-API enrichment orchestration**, not one shared table:

- `decision_part.rs` + `judilibre_zones.rs` share the `decision_zones` overlay — `official_decision_part` reads `decision_zones_json`; `enrich_decision_from_judilibre_with_client` writes via `upsert_decision_zones_with_client`.
- `legislation.rs` does **not** touch `decision_zones`. `collect_legislation_citations_payload` reads archived Judilibre `/decision` responses and writes `decision_legislation_citations` / `legislation_citation_resolutions`; `enrich_legislation_citations_payload` archives Legifrance responses in `official_api_responses` and updates `legislation_citation_resolutions`. It shares the official-API client and the response archive, not the zone cache.

Grouping them keeps the official-API enrichment coupling local instead of smeared across `retrieval` and `ingest`. **Resolved (codex, 2026-06-25):** keep `fetch_payload`/`emit_fetch` in `retrieval.rs` and the decision-part/zone helpers under `enrichment/`. `fetch_payload` is generic fetch until the optional `--part` branch (it parses `DecisionPart`, opens the index, checks `QueryReadinessGate::Fetch`, calls `fetch_documents_json`, handles no-results, and only then calls `annotate_fetched_parts`); the part path is a small separable overlay, so moving the whole fetch path under `enrichment/` would drag generic fetch logic along for no benefit. One caveat: `fetch_payload` calls `DecisionPart::parse` directly (`main.rs:4057`), so `enrichment/decision_part.rs` must expose a small public parsing surface (`DecisionPart` + its parse) for `retrieval.rs` to use.

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
   - `SearchArgs`, `FetchArgs`, `CiteArgs`, `RelatedArgs`, `ContextArgs`, `CompareArgs`, `QueryArgs`
   - `StatusArgs`, `InspectArgs`, `VersionsArgs`, `DiffArgs`
   - `JsonlArgs`, `ServeArgs`, `SyncArgs`
   - `ModelCommand`/`ModelSubcommand`, `EvalCommand`, `EvalSubcommand` (incl. `EvalFranceJurisZonesArgs`, `EvalTuneArgs`, `EvalRunArgs`, `EvalFranceLegiArgs`, `EvalFranceJurisArgs`, `EvalPhase1Args`), `IngestCommand`, `IngestSubcommand` (now 10 variants incl. the five new `EnrichZones`, `BuildZoneUnits`, `EmbedZoneUnits`, `CollectLegislationCitations`, `EnrichLegislationCitations`), `HelpCommand`/`HelpSubcommand`
   - CLI value enums such as `CliKind`, `CliSearchMode`, `CliOutputFormat`, `CliGroupBy`, `CliZone`, `CliEnrichZoneOrder`, archive/source enums (`CliArchiveSource`, `CliJuriSource`), and their `From`/`impl` conversions, plus the shared `default_*` functions (`default_top_k`, `default_group_by`, `default_related_*`, `default_compare_kind`, `default_cli_kind`, `default_search_mode`, `default_output_format`)
   - **Do NOT** move the `Session*Args` structs here. **Resolved (codex, 2026-06-25):** they are serde request DTOs (derive `Deserialize`, use `#[serde(default = ...)]`, no `Args`/`#[arg(...)]`; `main.rs:474`) consumed only by the session wrappers (e.g. `session_search_payload` `from_value::<SessionSearchArgs>` → builds `SearchArgs`, `main.rs:5299`), not clap parsers — so they move with `session.rs` in Phase 2. Keep the shared enums/`default_*` fns above in `args.rs` and import them into `session.rs`; don't relocate the DTOs just to avoid the import.
2. Move output helpers into `output.rs`:
   - `write_json`
   - `write_session_response`
   - `emit_error`
   - `emit_artifact`
3. Move `run` into `dispatch.rs`.
4. Leave payload functions in `main.rs` during this phase if needed; import them from `dispatch.rs` as `crate::search_payload`, etc. This creates a compilable intermediate step before deeper extraction.
5. Replace the special-case artifact write in `emit_eval` for France-LEGI with `emit_artifact(response, out_path)`. **Confirmed safe (codex, 2026-06-25):** both paths pretty-serialize the same `serde_json::Value`, create the parent dir, write `format!("{rendered}\n")`, then `write_json(&response)` to stdout — byte-identical for newline, pretty-print, file-vs-stdout, and object ordering (`emit_eval` France-LEGI branch `main.rs:1416`; `emit_artifact` `main.rs:1484`; `write_json` uses `to_writer_pretty` + one newline `main.rs:11968`). No test currently pins this writer branch. First add a small unit test on a factored file-render helper (given a `Value` + `out`, file == `to_string_pretty(value) + "\n"` and stdout == the same), then do the swap — an end-to-end `eval france-legi --out` test would need a ready index and is more brittle.

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
- `SessionRequest` / `SessionResponse`
- `dispatch_session_request`
- the session DTO structs (deferred from Phase 1): `SessionSearchArgs`, `SessionFetchArgs`, `SessionCiteArgs`, `SessionContextArgs`, `SessionRelatedArgs`, `SessionCompareArgs`, `SessionStatusArgs`, `SessionEvalPhase1Args`, `SessionModelFetchArgs`, `SessionDoctorArgs`, `SessionStatsArgs`, `SessionInspectArgs`, `SessionVersionsArgs`, `SessionDiffArgs` — they import the shared enums/`default_*` fns from `args.rs`
- `session_*_payload` wrappers (`session_search_payload`, `session_fetch_payload`, … `session_diff_payload`)

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

## Phase 3a: Retrieval Command Split (fetch excluded)

Target: move the high-frequency query commands that do **not** depend on the enrichment band out of the CLI monolith. `fetch_payload` is deliberately deferred to Phase 3c because it calls into the decision-part/Judilibre-zone helpers extracted in Phase 3b — moving `fetch` first would drag that whole band into `retrieval/` or force a second large move right after.

Create the `retrieval/` subtree (one submodule per command, re-exported from `retrieval/mod.rs`) rather than a single flat `retrieval.rs` — these payloads are not one responsibility (query/pagination/diagnostics vs citation state vs graph/structure reads), so a flat file becomes a second monolith. Move:

- `search.rs` — `search_payload`, `search_with_postgres`, `emit_search`, and the routing helpers used by `search_with_postgres`: `legi_citation_routing` + `LegiCitationRouting` (`main.rs:3578`/`3564`) and `is_iso_date` (`main.rs:3608`); plus the cursor/pagination helpers if only used here (`parse_search_cursor`, `search_pagination_value`, `validate_cursor_score`)
- `zone.rs` — `zone_search_payload`, `ensure_zone_retrieval_readiness` (the parallel zone retrieval index path)
- `cite.rs` — `cite_payload`, `emit_cite`, and the cite-only state helpers `classify_citation_state` / `annotate_valid_matches` (both have `cite_payload` as their only caller)
- `context.rs` — `context_payload`, `emit_context`
- `related.rs` — `related_payload`, `emit_related`
- `compare.rs` — `compare_payload`, `emit_compare`
- `expand.rs` — `expand_payload`, `emit_expand`

First extract the shared leaf-helper modules (see "Shared leaf-helper modules" above) as a small mechanical step, because `retrieval` is the first command module to move and it touches most of them. Concretely, before moving the payloads create: `ascii.rs`, `date.rs`, `query_support.rs`, `errors.rs`, `citation.rs`, and `legifrance_search.rs`; and move `PreparedQueryEmbedder` into `embedding_runtime.rs`. Notes on the trickier ones:

- `ascii.rs`: `find_ascii_ci` (`main.rs:3622`) + `rfind_ascii_ci` (`main.rs:3636`). `rfind_ascii_ci` is cross-module — called by `legi_citation_routing` (`main.rs:3594`, retrieval) **and** `heuristic_dispositif` (`main.rs:5150`, `enrichment/decision_part`). Move the pair together (`rfind_ascii_ci`'s doc references `find_ascii_ci`).
- `citation.rs`: do **not** move `parse_citation_target` / `ParsedCitationTarget` into `retrieval.rs` — it is shared by the eval path too (`france_juris_cite_documents`, `main.rs:2709`, as well as `cite_payload`, `main.rs:5180`). Move the full pure-parser closure with it (each helper *with its private dependencies*): `parse_citation_target` → `looks_like_nor`; `parse_article_number` → `article_number_token`; `detect_code_hint` → `contains_normalized_phrase`; plus `normalize_citation_text`, `parse_pourvoi`, `extract_known_source_uid`.
- `date.rs`: the calendar validators `is_valid_iso_date` + `days_in_month` + `is_leap_year` (shared retrieval+status); `validate_as_of` stays in `retrieval.rs` and imports them. The cheap `is_iso_date` (`main.rs:3608`) stays in `retrieval.rs` with `legi_citation_routing`.
- `query_support.rs`: `parade_query_text` + `validate_retrieval_options` (shared retrieval+eval).

Keep shared index-opening and query-readiness helpers in `index_runtime.rs` rather than letting retrieval own them. `parse_visa_citation` belongs with the legislation enrichment band (Phase 3b), not here.

Possible follow-up after the move:

- Consider moving pure argument-to-query conversion methods (`SearchArgs::retrieval_options`, `SearchArgs::decision_filters`) with `SearchArgs` in `args.rs`; retrieval can consume the methods without knowing clap details.

Validation:

- `cargo test -p jurisearch-cli`
- Any available focused CLI contract tests for search/cite/context/related/compare and the zone search path.

## Phase 3b: Enrichment Domain Split (new)

Target: extract the decision-part / Judilibre-zone / legislation-citation enrichment band (~4089-5178 plus the ingest-side payloads in Phase 5) into a cohesive `enrichment/` subtree, because it is shared by both the read path (`fetch --part --online`, Phase 3c) and the ingest path (`enrich-zones`, `collect/enrich-legislation-citations`, Phase 5). This phase runs **before** Phase 3c so `fetch_payload` and the ingest commands both move onto one shared copy of the zone-cache/citation logic.

Move to `enrichment/archive.rs` (shared `official_api_responses` archive helpers — assign these explicitly so submodules don't call back into `main.rs` or grow a sibling dependency):

- `archive_exchange` (`main.rs:4332`) — used by **both** `enrich_legislation_citations_payload` (`main.rs:4778`) and `enrich_decision_from_judilibre_with_client` (Judilibre `/search` + `/decision`, `main.rs:4892`, `main.rs:4908`)
- `sha256_hex` (`main.rs:4321`) — used by `archive_exchange` (`main.rs:4341`) and `archive_local_unsupported` (`main.rs:4398`)
- `archive_local_unsupported` (`main.rs:4372`) — Judilibre-only (called at `main.rs:4878`); keep it here next to the other archive helpers (or in `judilibre_zones.rs`), but it depends on `sha256_hex` so do not separate the two

Move to `enrichment/decision_part.rs`:

- `DecisionPart`, `ExtractedPart`, `annotate_fetched_parts`, `official_decision_part`, `extract_decision_part`, `heuristic_dispositif`, `heuristic_visa`, `collect_decision_summary`
- (depends on `ascii::rfind_ascii_ci` from Phase 3a via `heuristic_dispositif` — import it, do not re-copy)

Move to `enrichment/judilibre_zones.rs`:

- `is_judilibre_cassation_source`, `judilibre_zone_key`, `ZoneCacheAction`, `zone_cache_action`, `part_block_from_cached_zones`
- `enrich_decision_from_judilibre`, `enrich_decision_from_judilibre_with_client`, `find_matching_judilibre_id`, `normalize_judilibre_zones`, `zone_text_hash`, `cache_zone_status_with_client`
- `env_i64` (`main.rs:5086`) — single-module helper, used only by the zone-cache TTL handling here

Move to `enrichment/legislation.rs`:

- `ParsedVisaCitation`, `parse_visa_citation`, `legislation_citation_key`, `normalize_article_number`, `normalize_code_name`, `split_article_code`
- `legifrance_response_has_results`, `extract_first_href`, `strip_html_tags`
- `find_ci` (`main.rs:4422`) — single-module helper, used only by `extract_first_href` and `split_article_code` here
- `collect_legislation_citations_payload`, `enrich_legislation_citations_payload`
- **Not here:** `legifrance_code_search_body` / `sanitize_legifrance_query` / `LEGIFRANCE_QUERY_MAX_CHARS` move to the shared `legifrance_search.rs` leaf (Phase 3a) because `cite_payload`'s `--online` path also uses them; this module imports that leaf rather than owning the request-body builder.

Storage coupling differs by submodule and the split should respect it: `decision_part.rs` + `judilibre_zones.rs` read/write the `decision_zones` overlay (`decision_zones_json`, `upsert_decision_zones_with_client`); `legislation.rs` does not touch `decision_zones` at all — it reads archived Judilibre `/decision` responses and writes `decision_legislation_citations` / `legislation_citation_resolutions`. What `legislation.rs` and `judilibre_zones.rs` **do** share is the durable `official_api_responses` archive — both persist their raw upstream exchanges through `archive_exchange` — which is exactly why it (with `sha256_hex`) is factored into `enrichment/archive.rs` rather than duplicated. Keep the official-API client construction and the storage calls behind the existing storage/`jurisearch-official-api` APIs; this module is orchestration only. Do not move it into a library crate.

Validation:

- `cargo test -p jurisearch-cli`
- Focused contract tests for `ingest enrich-zones` and `ingest collect/enrich-legislation-citations` if fixtures are available.

## Phase 3c: Fetch Command Split

Target: move `fetch_payload` and `emit_fetch` into `retrieval/fetch.rs` now that the enrichment helpers they reach live in `enrichment/` (Phase 3b). `fetch_payload` directly uses `annotate_fetched_parts`, which in turn uses `official_decision_part`, `zone_cache_action`, and `part_block_from_cached_zones`. `fetch_payload` calls across to the enrichment module instead of owning the decision-part/zone-cache logic, so the read path and the ingest-side enrichment share one implementation.

Move to `retrieval/fetch.rs`:

- `fetch_payload`
- `emit_fetch`

Validation:

- `cargo test -p jurisearch-cli`
- Focused CLI contract tests for `fetch`, including `fetch --part`/`--online`.

## Phase 4: Status, Runtime, and Gate Split

Target: isolate release-gate truthfulness and runtime readiness from command plumbing.

Move to `embedding_runtime.rs`:

- `embedding_config_from_env`
- `loaded_embedding_config`
- model-cache helpers
- endpoint status/probe helpers
- embedding pool config/worker helpers
- the embedding-pool driver and its thin per-table wrappers: `embed_and_insert_with_pool` (generic driver), `embed_and_insert_chunks_with_pool`, and `embed_and_insert_zone_units_with_pool` (the zone-unit twin), plus the associated pool structs (`EmbeddingEndpointPoolConfig`, `EmbeddingPoolRun`, `EmbeddingBatchWork`, etc.). These are identical embedding concerns; keep them together here, not split across ingest. The `embed_*_payload` orchestrators stay in `ingest.rs` and call these.
- `PreparedQueryEmbedder` (`main.rs:3528`) — the query-time embedder built by both retrieval (`search_with_postgres`, `compare_payload`) and eval search paths; extract it here in Phase 3a (not `retrieval.rs`) since both command modules import it. (Listed under Phase 4's module but pulled early as a shared leaf per Phase 3a.)
- `ensure_embedding_runtime_ready`

Move to `index_runtime.rs`:

- `open_index`
- `open_index_for_bulk_ingest`
- `require_existing_index_dir`
- `require_configured_index_dir`
- `configured_index_dir`
- `ensure_query_readiness` (and `QueryReadinessGate` / `index_not_query_ready`)
- storage/embedding error mapping (`storage_error_object`, `embedding_error_object*`) is **not** owned here — it lives in the shared `errors.rs` leaf; `index_runtime.rs` imports it like every other command module

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
- `zone_retrieval_status_block` (new; reports the parallel zone-unit index health inside `status`)

Move to `gates/support.rs` (shared mechanics — factor these out so adding a gate does not re-duplicate artifact plumbing, per the SOLID/DRY review):

- the common artifact lifecycle: read an env-configured artifact path, parse JSON, normalize diagnostics, run a validator, set `state`/`artifact_error`, expose evidence/metrics/categories
- the generic dotted-pointer helpers (`artifact_pointer_value`/`_str`/`_f64`, `main.rs:10283`) and the shared category-validation result shape

Move to `gates/phase1.rs` and `gates/phase2.rs` (phase-specific floor logic and claims only — they call `gates::support`):

- `phase1_gate_payload`
- `phase1_external_benchmark_payload*`
- `phase1_france_legi_payload*`
- `phase1_*_artifact_errors`
- `phase2_gate_payload`
- `phase2_benchmark_payload*`
- `phase2_*_artifact_errors`
- gate-specific tests currently in the large `#[cfg(test)] mod tests`

Reasoning:

The gates are high-risk contractual code. They should not stay mixed with socket serving, model cache probing, archive ingestion, and command argument parsing. Splitting them also makes future reviews easier because gate changes will be localized. Keeping the load/parse/validate/report mechanics in `gates/support.rs` (not duplicated per phase) means a future Phase-3 gate reuses the plumbing and only adds its floors/claims.

Validation:

- `cargo test -p jurisearch-cli phase1`
- `cargo test -p jurisearch-cli phase2`
- `cargo test -p jurisearch-cli france_legi`
- Full `cargo test -p jurisearch-cli`

## Phase 5: Ingest Command Split

Target: move official-source ingestion orchestration out of the CLI entrypoint while keeping storage and parser APIs unchanged.

Move to `ingest.rs`:

- `emit_ingest`
- `sync_payload` and `emit_sync`/dispatch equivalent — keep it in `ingest.rs` **only while** it stays a thin incremental wrapper over `ingest_legi_archives_payload` / `ingest_juri_archives_payload` (`main.rs:1341`); if `sync` later gains independent delta/transactional-history semantics, split it into its own `sync.rs` rather than growing `ingest.rs`.
- archive manifest helpers:
  - `legi_archive_manifest`
  - `juri_archive_manifest`
  - `planned_archive_manifest`
  - `select_archives_to_process`
- `ingest_legi_archives_payload`
- `ingest_juri_archives_payload` (these two share an archive-run lifecycle — see SOLID/DRY follow-up #3, the `ArchiveIngestRun` runner, sequenced after this mechanical move)
- per-member processing helpers (`process_legi_archive_member*`, `process_juri_archive_member`, `record_*_member*`, batch flush helpers)
- quarantine helpers (`maybe_quarantine_payload`, `sanitize_quarantine_component`, parse-error classifiers)
- `embed_chunks_payload`
- `backfill_legi_hierarchy_payload`
- ingest run-id/date/source filter helpers (`default_legi_run_id`, `default_juri_run_id`, `unique_run_suffix`, `normalize_since`)

Move the new zone-unit pipeline to `ingest.rs` as well (it is ingest-side orchestration, even though it consumes the `enrichment/` helpers):

- `enrich_zones_payload`, `enrich_zone_page_concurrently`, `worker_outcomes_or_errors`, `ZoneEnrichOutcome` (`main.rs:7283`, local to these `enrich-zones` helpers) (the `enrich-zones` Judilibre backfill)
- `derive_zone_unit_rows`, `build_zone_units_payload` (the `build-zone-units` derivation)
- `embed_zone_units_payload` (the `embed-zone-units` orchestrator). It calls `embed_and_insert_zone_units_with_pool`, which moves to `embedding_runtime.rs` in Phase 4 alongside its `embed_and_insert_chunks_with_pool` twin and the shared pool driver — do **not** move the `*_with_pool` wrapper into ingest, or embedding runtime would gain a backward dependency on ingest.

The legislation-citation collection/enrichment commands (`collect-legislation-citations`, `enrich-legislation-citations`) dispatch from `emit_ingest`, but their payload bodies should live in `enrichment/legislation.rs` (Phase 3b). Keep `emit_ingest` thin: match the subcommand, call across to the enrichment module.

Dependency caution:

This module will need both parser crates and storage projection/accounting imports, plus (for the zone pipeline) the `enrichment/` helpers and the official-API client. Keep it in the CLI crate first because it is orchestration: it converts CLI flags to parser/storage/enrichment calls and JSON artifacts. Do not move it into `jurisearch-ingest` unless a non-CLI caller needs the same orchestration API.

Validation:

- `cargo test -p jurisearch-cli`
- `cargo test -p jurisearch-ingest`
- `cargo test -p jurisearch-storage ingest_accounting`
- `cargo test -p jurisearch-storage zone_units`
- Archive subset tests if local fixtures are available.

## Phase 6: Split CLI Contract Tests

Target: reduce `crates/jurisearch-cli/tests/cli_contract.rs` from 5,049 lines into domain-focused integration suites.

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

## SOLID/DRY Design Follow-ups (post-mechanical)

The file split above is a behavior-preserving *mechanical* decomposition; it reduces merge conflicts but does not by itself fix the structural duplication baked into the current command surface. A codex SOLID/DRY review (2026-06-25, GO; `reviews/2026-06-25-refactoring-plan-solid-dry-codex-review.md`) flagged the following. **Sequence these AFTER the mechanical moves** so the moved-symbol diffs stay reviewable; each is its own focused, behavior-preserving commit with the contract tests as the guard. None requires a command-handler trait.

1. **Command inventory / registry (OCP).** Today, adding a command or argument forces coordinated edits across the clap `Command` enum (`main.rs:186`), `dispatch::run` (`main.rs:1231`), `dispatch_session_request` (`main.rs:9398`), `contract::COMMANDS` (`jurisearch-core/src/contract.rs:66`), `SESSION_EXCLUDED_COMMANDS` (`contract.rs:255`), and `compiled_schema()` (`schema.rs:5`). Introduce a single internal command descriptor table — name, session availability, request/response schema names, handler entrypoint where practical — and derive `agent_help`, session-exclusion checks, and schema command listing from that one inventory instead of parallel literal lists. Keep clap derive types; this is metadata unification, not a handler trait.

2. **Shared command request structs (DRY — biggest hole).** The `Session*Args` DTOs duplicate nearly every field of their clap `*Args` twin (e.g. `SessionSearchArgs` vs `SearchArgs`, `main.rs:473`/`295`) and every `session_*_payload` manually rebuilds the clap struct field-by-field (`main.rs:5309`, `5333`, `5352`, `5370`, `5389`, `5407`). Introduce shared internal request structs (`SearchRequest`, `FetchRequest`, `CiteRequest`, …) in `request.rs`: produced by `TryFrom<SearchArgs>` for the one-shot/clap path and by `serde` deserialization for the session path. Payload functions take the shared request type, not clap structs. This collapses the parallel `*_payload`/`session_*_payload` argument surface without a trait. (Refines the Phase 2/Phase 3a boundaries: the `Session*Args` still live in `session.rs`, but the conversion target is the shared request.)

3. **Generic archive-ingest runner (DRY).** `ingest_legi_archives_payload` (`main.rs:5843`) and `ingest_juri_archives_payload` (`main.rs:6209`) share the whole archive-run lifecycle — plan, start run, select archives, read/flush member batches, fatal-error handling, manifest update, terminal status, replay-snapshot refresh, response shaping — differing only in source-specific manifest/counter/member parsing. Introduce a private `ArchiveIngestRun` (or a small generic runner) in `ingest.rs` that owns the common lifecycle and delegates source-specific bits via parameters / a tiny source adapter. Keep it private to `ingest`; do not promote it into `jurisearch-ingest` yet.

The remaining review points are already reflected in the module map and phases above: `retrieval/` is split per command (SRP — search/zone/fetch/cite/context/related/compare/expand, see Phase 3a/3c) rather than one flat file; `eval/` is split per benchmark family (generic/france_legi/france_juris/zones/artifact) with only genuinely shared metric/category/artifact helpers factored out; and `gates/support.rs` holds the shared artifact-load/parse/dotted-pointer/validator-result mechanics so `phase1.rs`/`phase2.rs` keep only floor logic and claims (see Phase 4).

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

Priority: this is now the **highest-priority secondary target** (1,418 lines, up from 1,101 on 2026-06-24) because the legislation-citation enrichment work added Legifrance request/response surface on top of the existing PISTE/Judilibre client. Consider doing it right after the CLI phases rather than last.

Natural split:

```text
config.rs
client.rs        # PISTE/Judilibre
legifrance.rs    # Legifrance OAuth/search EXCHANGE client (generic HTTP/token), e.g. PisteClient::legifrance_search_exchange
auth.rs
retry.rs
error.rs
```

This should be straightforward because the public API is already centered on `OfficialApiConfig`, `PisteClient`, `RetryPolicy`, and `OfficialApiError`. Scope `legifrance.rs` in this crate to the **generic** exchange surface only: the crate currently owns `PisteClient::legifrance_search_exchange` (token + HTTP). The citation-specific request body and response interpretation — `legifrance_code_search_body`, `sanitize_legifrance_query`, `legifrance_response_has_results` — currently live in the CLI (`crates/jurisearch-cli/src/main.rs`) and should stay there in `enrichment/legislation.rs` unless a non-CLI caller needs them. Do not duplicate the request body in both crates.

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

**Resolved (codex, 2026-06-25): split it — after the CLI move, not before.** The original draft guessed this was generated/compiled-contract style; it is not. `compiled_schema()` is a hand-maintained literal `json!({ ... })` tree (`schema.rs:5`), and it is high-churn — 36 commits touched it since 2026-06-20 (CLI milestones, gates, eval artifacts, zone retrieval). High churn + hand-maintained is exactly the profile worth splitting. It is still secondary to the 13k-line CLI module, so sequence it after the CLI extraction.

Suggested file-level fragmentation (keep `compiled_schema()` assembling one `serde_json::Map` in an explicit order):

```text
schema/
  mod.rs       # compiled_schema(), root assembly, exit_codes, error_object, session_envelope, common_enums
  search.rs    # search/compare/fetch/cite/context/related/expand
  admin.rs     # status/model/setup/doctor/stats/inspect/versions/diff/sync/help/serve/ingest
  eval.rs      # phase/eval/tune/France-LEGI/France-juris benchmark schemas
  gates.rs     # Phase 1/Phase 2 gate + benchmark gate support schemas
```

Two hard constraints from codex:
- **Golden equality test first.** Before splitting, capture the current `serde_json::to_string_pretty(&compiled_schema())` as a golden fixture and add a test asserting the split output is byte-identical. Keep the existing `every_command_schema_name_resolves` invariant (`schema.rs:1042`) too.
- **`$ref` paths are global** (`#/schemas/...`). Split at the file level only — do **not** nest domain schemas under separate sub-objects, or every `$ref` would have to be rewritten. The emitted JSON shape must stay unchanged.

## Visibility and API Policy

- Prefer `pub(crate)` for new CLI modules.
- Preserve existing public crate exports for `jurisearch-ingest`, `jurisearch-storage`, `jurisearch-embed`, and `jurisearch-official-api`.
- Avoid moving CLI orchestration into library crates unless a real non-CLI caller exists.
- Avoid introducing traits for command handlers in the first pass. Straight modules and functions are enough.
- Keep `ErrorObject` boundaries intact. Commands currently return `Result<Value, ErrorObject>` internally and emit JSON/exit at the outer layer; that is a good boundary to preserve.

## Suggested Commit Plan

Mechanical decomposition (behavior-preserving file moves):

1. `cli: split args and output helpers from main`
2. `cli: move dispatcher into dispatch module`
3. `cli: move jsonl session and serve protocol modules`
4. `cli: extract shared leaf helpers (citation/ascii/date/errors/query_support/legifrance_search + PreparedQueryEmbedder)` (Phase 3a prerequisite)
5. `cli: move retrieval command payloads except fetch into retrieval/ submodules (incl. zone_search)` (Phase 3a)
6. `cli: extract enrichment domain (decision-part / judilibre-zones / legislation)` (Phase 3b)
7. `cli: move fetch command into retrieval/fetch.rs onto enrichment helpers` (Phase 3c)
8. `cli: move status/runtime helpers (incl. zone_retrieval_status_block)`
9. `cli: move phase gates + gates/support.rs artifact validators`
10. `cli: move eval payloads into eval/ subtree (generic/france_legi/france_juris/zones/artifact)`
11. `cli: move ingest command orchestration (incl. zone-unit pipeline)`
12. `cli: split integration contract tests`
13. `official-api: split piste/legifrance/auth/retry/error` (bumped earlier; highest secondary)
14. `ingest: split legi parser internals`
15. `ingest: split juri parser internals`
16. `storage: split retrieval SQL emitters`
17. `storage: split projection write paths`
18. `core: split schema.rs into schema/ fragments (golden byte-identical test)`

SOLID/DRY structural follow-ups (post-mechanical — only after the moves above so moved-symbol diffs stay reviewable; each is its own behavior-preserving commit guarded by the contract tests):

19. `cli: introduce command inventory/registry (unify dispatch + session-exclusion + schema listing)` [OCP]
20. `cli: shared command request structs + TryFrom<*Args> (collapse session-DTO rebuild)` [DRY]
21. `cli/ingest: introduce ArchiveIngestRun runner (collapse legi/juri archive lifecycle)` [DRY]

Each source-split commit should compile independently and should avoid semantic changes except the small `emit_artifact` duplication cleanup if done with a focused test. Ordering notes: the shared leaf helpers (commit 4) come out **first** so no later command-module move has to reach back into `main.rs` for a cross-cutting helper; the enrichment extraction (commit 6 / Phase 3b) lands **before** the `fetch` move (commit 7 / Phase 3c) and the ingest move (commit 11), so both the read path (`fetch --part`) and the ingest path (`enrich-zones`, legislation citations) call into one shared module instead of duplicating the zone-cache logic. The non-fetch retrieval commands (commit 5 / Phase 3a) have no enrichment dependency and can move once the leaves exist. The follow-ups (19-21) are deliberately last: they introduce small new abstractions (registry, shared request structs, archive runner) and are easier to review once the code already lives in its target modules.

## Risk Areas

- Gate truthfulness: Phase 1/2 status gates validate artifacts, floors, evidence, and routing claims. Move tests with the exact gate functions.
- Session parity: session commands are intended to match one-shot command payloads where supported. Keep session wrappers close to payload builders or add explicit tests when moving.
- Artifact output: `eval france-legi`, `eval france-juris`, `eval france-juris-zones`, `eval run`, and `eval tune` all print and optionally write JSON artifacts. Any output-helper consolidation must preserve newline and pretty-print behavior. The zone benchmark (`zone_benchmark_artifact`) is a newer addition — keep its artifact validator next to it.
- Enrichment coupling (new): the decision-part fetch path and Judilibre zone caching share the `decision_zones` storage overlay and the official-API client; the Legifrance legislation resolution shares the official-API client and response archive (`official_api_responses` + `legislation_citation*` tables) but **not** `decision_zones`. The decision-part/zone helpers straddle read (`fetch`) and ingest (`enrich-zones`) paths, so extract the shared band into `enrichment/` (Phase 3b) **before** moving `fetch` (Phase 3c) and the ingest commands (Phase 5), or the zone-cache logic will be split or duplicated across modules. Preserve the cached `decision_zones` overlay semantics — see the standing rule to keep that cache on promotion.
- Network/idempotency: `enrich-zones` and `enrich-legislation-citations` make rate-limited PISTE/Legifrance calls and are resumable; moving them must not change run-id derivation, since-filter handling, concurrency bounds, or the archived-response read path (`official_api_responses`).
- Ingest accounting: ingest commands coordinate storage run lifecycle, manifest updates, member status, quarantine, and query-readiness invalidation. Avoid moving storage transaction ownership across crate boundaries in the first refactor.
- Working tree (2026-06-25): apart from this plan document, the source tree is clean, so once this plan edit is committed the large moves can start from `HEAD`. Re-confirm cleanliness before each phase so mechanical moves stay reviewable.

## Definition of Done

The refactor is done when:

- `crates/jurisearch-cli/src/main.rs` is under roughly 250 lines and contains only module declarations, `main`, and trivial wiring.
- No single production file remains above roughly 2,000 lines unless intentionally generated or contract data.
- Existing CLI JSON contracts and exit behavior are unchanged.
- `cargo fmt --check` passes.
- `cargo test -p jurisearch-cli` passes after every CLI-phase commit.
- Broader crate tests pass at phase boundaries.
- CodeGraph can show command-specific context without returning most of the CLI file for unrelated tasks.
