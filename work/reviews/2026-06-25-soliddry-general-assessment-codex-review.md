# SOLID/DRY assessment: jurisearch current `main`

Scope: holistic source review of the current working tree, not a diff review. I read the CLI command/session contract, recent `request.rs` / `ingest/run.rs` / `CommandSpec` abstractions, large modules, retrieval/eval/ingest paths, storage runtime/migrations, and concrete external-client boundaries. I did not run tests; this is a static maintainability review.

## Findings

### Medium: `embedding_runtime.rs` has become three modules in one

Principle: SRP / ISP / DRY

Location: `crates/jurisearch-cli/src/embedding_runtime.rs:1`, `crates/jurisearch-cli/src/embedding_runtime.rs:237`, `crates/jurisearch-cli/src/embedding_runtime.rs:622`, `crates/jurisearch-cli/src/embedding_runtime.rs:873`, `crates/jurisearch-cli/src/embedding_runtime.rs:1083`, `crates/jurisearch-cli/src/embedding_runtime.rs:1298`

The file advertises itself as query embedding, config loading, status probes, and bulk endpoint-pool scheduling in one module. Those are visible as distinct blocks: query embedder at lines 7-43, the generic concurrent pool at lines 237-355, TOML/env config structs and loading at lines 622-949, model-cache/status helpers at lines 1083-1267, and readiness at lines 1298-1305. Each block is internally reasonable, but the module boundary is now too broad: changes to user config parsing, model-cache diagnostics, and bulk scheduler error semantics all land in the same 1300-line file with `use crate::*` at line 5.

Why it matters: the risky code here is not just "large"; it has different invariants. Pool scheduling must preserve stop/error/insert semantics, config loading must preserve env/TOML precedence, and model status must avoid network side effects. Keeping those in one module increases review cost and makes unrelated changes look coupled.

Recommendation: split this file mechanically into `embedding_runtime/config.rs`, `embedding_runtime/pool.rs`, and `embedding_runtime/status.rs`, leaving `PreparedQueryEmbedder` in `mod.rs` or `query.rs`. Keep the public `pub(crate)` surface and tests stable; do not introduce an embedding-provider trait until a second production provider exists. This is worth doing because it reduces actual review blast radius without changing behavior.

### Medium: the eval families duplicate category-loop and production-search scaffolding

Principle: DRY / SRP

Location: `crates/jurisearch-cli/src/eval/france_legi.rs:40`, `crates/jurisearch-cli/src/eval/france_legi.rs:60`, `crates/jurisearch-cli/src/eval/france_legi.rs:83`, `crates/jurisearch-cli/src/eval/france_legi.rs:262`, `crates/jurisearch-cli/src/eval/france_juris.rs:40`, `crates/jurisearch-cli/src/eval/france_juris.rs:79`, `crates/jurisearch-cli/src/eval/france_juris.rs:174`, `crates/jurisearch-cli/src/eval/zones.rs:41`, `crates/jurisearch-cli/src/eval/zones.rs:77`

`france_legi` manually implements three category loops; `known_item` and `temporal` are nearly the same shape, and `cross_reference` is the same control flow with a different hit predicate. `france_juris` and `zones` then repeat the broader pattern: open once, get gold JSON, iterate qrels, resolve documents through the production path, count hits/done, shape a `CategoryResult`. The per-query production wrappers also duplicate hand-built `SearchRequest` construction in `france_legi_search_documents` and `france_juris_search_documents`.

Why it matters: the correctness risk is benchmark drift. These runners are evidence-generating code, so small fixes to "skip malformed qrel", "treat `NoResults` as miss", "unique document ids", or "top_k/overfetch semantics" need to land consistently across gate and advisory benchmarks. Today that consistency depends on remembering several similar loops.

Recommendation: add a small eval helper, not a full benchmark framework: one function that scores a qrel array with a document resolver and hit predicate, returning `{metric, queries}` plus optional per-backend counts. Add a `SearchRequest` builder for benchmark document retrieval. Keep artifact shaping separate because each artifact has a different contract. This is a practical DRY cleanup and worth a follow-up.

### Medium: `search_with_postgres` is the main retrieval god-function candidate

Principle: SRP / ISP

Location: `crates/jurisearch-cli/src/retrieval/search.rs:157`, `crates/jurisearch-cli/src/retrieval/search.rs:173`, `crates/jurisearch-cli/src/retrieval/search.rs:198`, `crates/jurisearch-cli/src/retrieval/search.rs:233`, `crates/jurisearch-cli/src/retrieval/search.rs:268`, `crates/jurisearch-cli/src/retrieval/search.rs:305`

`search_with_postgres` is explicitly allowed to have many arguments, but the smell is deeper than arity. It performs readiness gating, temporal/kind/group limit setup, query embedding, storage query construction, structured citation intent routing, hybrid fallback, response mutation, cursor pagination, routing diagnostics, detailed diagnostics, and no-results mapping in one body. This makes the central production path harder to extend safely when adding a new retrieval mode or routing branch.

Why it matters: the function is reused by the user-facing `search`, `eval france-legi`, and `eval france-juris` paths. Any future retrieval mode, cursor rule, or routing branch will touch a high-stakes function where behavior preservation is important.

Recommendation: introduce narrow data helpers rather than a mode trait hierarchy. A good split would be `SearchExecution` / `SearchContext` for the already-open index, prepared embedder, readiness policy, and request-derived limits; `run_hybrid_candidates`; `run_structured_citation_or_fallback`; and `apply_search_response_envelope`. This keeps Postgres concrete and still makes routing, candidate execution, and response shaping independently reviewable.

### Low/Medium: command extension still requires parallel edits, though this is mostly acceptable for this CLI

Principle: OCP

Location: `crates/jurisearch-cli/src/args.rs:36`, `crates/jurisearch-cli/src/dispatch.rs:22`, `crates/jurisearch-cli/src/session.rs:127`, `crates/jurisearch-core/src/contract.rs:72`, `crates/jurisearch-core/src/contract.rs:287`

`CommandSpec.session_excluded` is a good fix for one specific drift source: the session-exclusion contract now comes from `COMMANDS` via `command_session_excluded`. But adding a new command still requires edits in the Clap enum, one-shot dispatch, session dispatch if session-callable, `COMMANDS`, and often `request.rs` plus schemas. That is not automatically bad in a byte-identical CLI contract, but it is still a parallel-structure seam.

Why it matters: the next implemented command can easily be advertised, parsed, and session-routed inconsistently unless tests catch it. The current explicit match arms are readable and preserve exact error precedence, so replacing them with a dynamic registry would be over-engineering.

Recommendation: keep explicit dispatch. Add or strengthen contract tests that assert every implemented, session-available `CommandSpec` has a session dispatch arm, and every one-shot-only command has `session_excluded: true`. Treat `CommandSpec` as contract metadata, not as a runtime dispatcher.

### Low/Medium: repeated index-open/readiness/JSON parsing preambles are still copy-pasted

Principle: DRY

Location: `crates/jurisearch-cli/src/index_runtime.rs:7`, `crates/jurisearch-cli/src/index_runtime.rs:37`, `crates/jurisearch-cli/src/index_runtime.rs:73`, `crates/jurisearch-cli/src/retrieval/fetch.rs:30`, `crates/jurisearch-cli/src/retrieval/context.rs:20`, `crates/jurisearch-cli/src/retrieval/related.rs:26`, `crates/jurisearch-cli/src/retrieval/compare.rs:28`, `crates/jurisearch-cli/src/eval/france_legi.rs:13`, `crates/jurisearch-cli/src/eval/france_juris.rs:20`

The repo already has the right primitives: `require_existing_index_dir`, `open_index`, and `ensure_query_readiness`. The payloads still repeat the three-call sequence and the same `serde_json::from_str(...).map_err(dependency_unavailable)` bridge from storage JSON strings to CLI `Value`s.

Why it matters: this is not a design crisis, but it creates small, repeated opportunities for error-precedence drift and inconsistent error messages when new retrieval/admin payloads are added.

Recommendation: add lightweight helpers such as `open_query_index(index_dir, QueryReadinessGate)` and `parse_storage_json(response, context)`. Do not hide command-specific validation or no-results checks; those are rightly local to each payload.

### Low: `use crate::*` is tolerable binary glue, but it amplifies the broad modules

Principle: ISP / coupling visibility

Location: `crates/jurisearch-cli/src/main.rs:127`, `crates/jurisearch-cli/src/session.rs:20`, `crates/jurisearch-cli/src/request.rs:13`, `crates/jurisearch-cli/src/embedding_runtime.rs:5`, `crates/jurisearch-cli/src/eval/generic.rs:3`, `crates/jurisearch-cli/src/ingest/legi.rs:3`

The hub pattern is deliberate, and for a single-binary crate it is not inherently wrong. It did keep the refactor low-risk. The cost is that leaf modules do not show their real dependencies; this is most visible in already-broad files like `embedding_runtime.rs`, `eval/generic.rs`, and ingestion modules.

Why it matters: wide imports make coupling harder to audit and can mask accidental dependencies during future extraction. This is a maintainability smell, not a current correctness issue.

Recommendation: do not churn the whole CLI just to remove globs. As modules are split or materially touched, narrow imports in the changed leaf modules. Prioritize `embedding_runtime` and eval helpers; leave small command modules alone unless they are already being edited.

## Non-findings and recent abstraction assessment

`request.rs` is a sound abstraction. It gives the one-shot and session paths a shared deserialization/validation target for the commands that actually had duplicated fields, and its comments clearly explain why index-dir-only commands keep small DTOs. The remaining per-command `into_request` mappings are acceptable because they preserve Clap/session defaults explicitly (`crates/jurisearch-cli/src/request.rs:1`, `crates/jurisearch-cli/src/request.rs:76`, `crates/jurisearch-cli/src/session.rs:33`).

`ingest/run.rs` is also the right line. It extracts the shared member batching loop and leaves source-specific lifecycle, manifest, status recomputation, backfill, and response shaping visible in LEGI and JURI (`crates/jurisearch-cli/src/ingest/run.rs:1`, `crates/jurisearch-cli/src/ingest/legi.rs:109`, `crates/jurisearch-cli/src/ingest/juri.rs:95`). A generic `ArchiveIngestAdapter` would hide the most auditable parts of ingestion. If a third archive family lands, revisit only manifest/coverage shaping, not the whole run lifecycle.

`CommandSpec.session_excluded` is a good OCP improvement for the exact problem it solves. It unifies the "advertised but not session-callable" decision and avoids another manual exclusion list (`crates/jurisearch-core/src/contract.rs:57`, `crates/jurisearch-core/src/contract.rs:283`). It should not be stretched into a dynamic command registry.

`args.rs` is large but cohesive. It owns Clap/serde boundary definitions, value enums, defaults, and conversions (`crates/jurisearch-cli/src/args.rs:1`, `crates/jurisearch-cli/src/args.rs:36`, `crates/jurisearch-cli/src/args.rs:786`). Splitting it now would mostly move definitions around.

`ingest/src/legi/parser.rs` is large but cohesive. It has root detection, root-specific event loops, assignment helpers, and raw-to-domain builders (`crates/jurisearch-ingest/src/legi/parser.rs:58`, `crates/jurisearch-ingest/src/legi/parser.rs:112`, `crates/jurisearch-ingest/src/legi/parser.rs:185`, `crates/jurisearch-ingest/src/legi/parser.rs:301`, `crates/jurisearch-ingest/src/legi/parser.rs:524`). There is repeated XML event-loop shape, but the root-specific extraction logic is different enough that a generic visitor would be brittle.

`storage/src/runtime.rs` is broad but cohesive around owning an embedded Postgres lifecycle: discovery, init/start, locks, runtime config, psql execution, shutdown, and error reporting (`crates/jurisearch-storage/src/runtime.rs:32`, `crates/jurisearch-storage/src/runtime.rs:86`, `crates/jurisearch-storage/src/runtime.rs:170`, `crates/jurisearch-storage/src/runtime.rs:261`, `crates/jurisearch-storage/src/runtime.rs:432`, `crates/jurisearch-storage/src/runtime.rs:673`). I would not invert this behind traits for this local-first app.

`storage/src/migrations.rs` is append-only SQL plus a small runner, which is an appropriate shape for schema history (`crates/jurisearch-storage/src/migrations.rs:19`, `crates/jurisearch-storage/src/migrations.rs:704`, `crates/jurisearch-storage/src/migrations.rs:757`). The file is long because schema history is long, not because responsibilities are mixed.

DIP/LSP are mostly healthy for the product shape. Storage is intentionally concrete `ManagedPostgres` at command boundaries, while lower-level write paths use `postgres::GenericClient` where transactions/tests/thread workers need it (`crates/jurisearch-cli/src/ingest/legi.rs:390`, `crates/jurisearch-cli/src/enrichment/judilibre_zones.rs:100`). The embedding and official API clients are concrete (`crates/jurisearch-embed/src/client.rs:84`, `crates/jurisearch-official-api/src/client.rs:5`), which is the right call until there is a second real implementation. Adding traits now would likely add mocks and indirection without simplifying production code.

## Executive Summary

The refactor materially improved the codebase: the old monolith is gone, the request structs remove a real one-shot/session drift risk, the archive batching helper is narrow and well-tested, and the command inventory now owns session exclusion. Remaining SOLID/DRY issues are not release blockers, but there are follow-ups worth doing: split `embedding_runtime.rs`, factor the eval category scoring/search scaffolding, decompose `search_with_postgres`, add contract tests around command/session alignment, and add small helpers for repeated index/readiness/JSON preambles. I would avoid broad traitification of storage, embedding, official API, or archive ingestion lifecycle; those would be academically cleaner but lower-value for this local-first single binary.

VERDICT: FIXES_REQUIRED
