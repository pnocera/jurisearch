# Code Review: CLI Enhancement Implementation Plan

## Findings

### HIGH: Phase 0 does not actually make every advertised schema resolvable before adding the invariant

The plan's definition of done says every `COMMANDS` entry must have request and response schema bodies, and T0.5 adds an invariant that should enforce that (`CLI-IMPLEMENTATION-PLAN.md:24-32`, `:88-93`). But the Phase 0 work items only add `SearchResponse.routing` and `EvalFranceLegiRequest/Response` (`CLI-IMPLEMENTATION-PLAN.md:65-78`). That leaves current contract entries still pointing at schema names that are not defined in `compiled_schema()`: for example `FetchRequest`, `RelatedRequest`, `RelatedResponse`, `IngestRequest`, `SyncRequest`, `SyncResponse`, `SessionRequest`, `SessionResponse`, `HelpAgentRequest`, `HelpAgentResponse`, `HelpSchemaRequest`, and `HelpSchemaResponse` are named in `crates/jurisearch-core/src/contract.rs:66-178`, while `crates/jurisearch-core/src/schema.rs:77-559` only defines a subset and has `session_envelope` outside `schemas`.

As written, T0.5 either fails immediately or gets weakened to ignore exactly the drift it is supposed to prevent. The "first concrete step" also claims T0.1-T0.3 unblocks the parity/invariant work (`CLI-IMPLEMENTATION-PLAN.md:212-216`), but it does not close the full schema-body backlog.

Recommendation: expand T0.2 into "schema completeness for all current contract names", not just `SearchResponse.routing`. Add schema bodies or explicit aliases for every `request_schema` and `response_schema` currently listed in `COMMANDS`, including session/help envelope schemas, before or as part of T0.5. Make the acceptance criterion a generated check over every schema name in `COMMANDS`.

### HIGH: `db start` is planned against an ownership model that stops Postgres on drop

T1.4 says `db {start,stop,status}` should use the existing `runtime.rs` `start_durable`/`stop` APIs and let `db start/stop` manage durable Postgres (`CLI-IMPLEMENTATION-PLAN.md:134-143`). In the live runtime, `ManagedPostgres::start_durable()` starts PG, holds an advisory lock, and returns an owning handle (`crates/jurisearch-storage/src/runtime.rs:163-229`). `ManagedPostgres` then calls `stop()` in `Drop` (`crates/jurisearch-storage/src/runtime.rs:326-330`). A one-shot `jurisearch db start` implemented by just calling `start_durable()` would start the database, finish the command, drop the handle, and stop it again.

This is not just an implementation detail; it changes the shape of the lifecycle feature. To retire manual `pg_ctl`, the CLI needs either a long-running owner (`serve`/daemon), a detached/supervised process, or a separate attach/status/stop model that can discover an existing PG instance without taking the same ownership path.

Recommendation: split T1.4 into a design task for runtime ownership. Define whether `db start` launches a daemon, writes connection metadata, uses a supervisor, or becomes an alias for `serve`. Add acceptance tests proving `db start` returns while PG remains reachable and `db stop` can stop that same instance. Do not describe `start_durable`/`stop` as sufficient until the drop/lock semantics are changed or bypassed intentionally.

### MEDIUM: The help pass claims "all existing commands" but scopes and tests only a subset

T0.1 says it is a help-text pass on all existing commands, but its explicit touchpoints cover only `SearchArgs`, `FetchArgs`, `CiteArgs`, `RelatedArgs`, `ContextArgs`, `QueryArgs`, `StatusArgs`, and `SyncArgs`, and its snapshot acceptance only checks `search`/`fetch`/`cite`/`related`/`context` (`CLI-IMPLEMENTATION-PLAN.md:57-63`). Current top-level commands also include `model`, `setup`, `session`, `batch`, `ingest`, `eval`, and `help` (`crates/jurisearch-cli/src/main.rs:123-154`), and nested surfaces include `eval phase1`, `eval france-legi`, model subcommands, and ingest subcommands.

That leaves a path for the first Phase 0 PR to pass while important agent-facing entry points still have shallow or inconsistent help. It also weakens the stated milestone that `--help`, `help agent`, and `help schema --json` describe the entire real surface (`CLI-IMPLEMENTATION-PLAN.md:95-97`).

Recommendation: enumerate every top-level and nested command path in T0.1 and in the snapshot matrix. At minimum include `model fetch`, `setup`, `session`, `batch`, each `ingest` subcommand, `eval phase1`, `eval france-legi`, `sync`, and both help subcommands. If some commands intentionally do not get rich examples, record that exception explicitly.

### MEDIUM: Document-level search needs a real pagination/cursor design, not post-page dedupe

T1.2 proposes `search --group-by {chunk,document}` and says document grouping "aggregates to first unique article preserving rank" while keeping cursor semantics consistent (`CLI-IMPLEMENTATION-PLAN.md:113-121`). The current retrieval cursor is chunk-based: storage returns chunk candidates and the cursor predicate compares `fused_score` plus `chunk_id` (`crates/jurisearch-storage/src/retrieval.rs:324-335`), while the CLI truncates candidates to `top_k` and bases `next_cursor` on the last displayed chunk (`crates/jurisearch-cli/src/main.rs:1440-1468`).

If document grouping is implemented after the current page is fetched, duplicate chunks can collapse the visible page below `top_k`, skip later documents behind duplicate chunks, and produce a next cursor that resumes from a chunk rather than the last emitted document. That would make `--group-by document` look correct in small tests while remaining lossy under dense/hybrid duplicate-heavy results.

Recommendation: make T1.2 require document grouping inside the ranking query or an explicit overfetch-and-fill loop with a document-level cursor contract. Define the response cursor fields separately for `chunk` and `document` grouping, and add a pagination fixture where multiple top-ranked chunks belong to the same document and the first page still returns `top_k` unique documents without skipping the next page.

### MEDIUM: Request-scoped tuning is underspecified for sessions and future `serve`

T2.1 adds `search --rrf-lexical-weight --rrf-dense-weight --probes` as overrides for env-driven `rrf_weights()` (`CLI-IMPLEMENTATION-PLAN.md:152-155`). Today RRF weights are read from process environment in `crates/jurisearch-storage/src/retrieval.rs:21-35`, and IVFFlat probes are hard-coded by emitting `SET ivfflat.probes = 4` inside `hybrid_candidates_json()` (`crates/jurisearch-storage/src/retrieval.rs:101-118`). That is workable for a single one-shot process, but it is the wrong abstraction for warm sessions and especially for T2.4 `serve`, where requests may need different weights/probes concurrently.

Without a request-scoped storage contract, implementing CLI flags by mutating env or process/session state would make tuning non-deterministic in a server and hard to compare in `eval tune`.

Recommendation: add T2.1 touchpoints for `HybridCandidateQuery` (or a retrieval options struct), schema/session arguments, and SQL-local probe handling. Flags should flow as immutable per-request options through `search_payload` into storage; `eval tune` should sweep those options without changing process env.

### LOW: The read-only SQL escape hatch needs stronger safety than "single SELECT"

T2.2 proposes `sql --read-only "<SELECT...>"` and an acceptance test that rejects anything but a single SELECT (`CLI-IMPLEMENTATION-PLAN.md:157-162`, `:197`). The existing escape hatch underneath is `ManagedPostgres::execute_sql(&str)`, which shells arbitrary SQL through `psql` (`crates/jurisearch-storage/src/runtime.rs:237-239`). A string-level "single SELECT" check is not enough for a safe escape hatch: PostgreSQL SELECTs can call volatile functions, CTEs can be surprising, and parser bypasses around comments/dollar quoting/multiple statements are easy to miss if this is hand-rolled.

Recommendation: either keep raw SQL out of the first implementation and prioritize typed `inspect`/`stats`/`explain`, or make the plan require database-enforced read-only execution: a read-only transaction, timeout, restricted role where possible, server-side statement splitting/parsing rather than ad hoc string checks, and tests for CTEs, comments, semicolons, COPY, function calls, and DDL/DML attempts.

## Summary

The plan is directionally sound and it incorporates the earlier analysis review's main corrections. The blockers are in the implementation sequencing: Phase 0 still leaves many contract schema names unresolved, and the DB lifecycle task assumes a runtime ownership model that currently tears the database down when the CLI command exits. Fix those before using this plan as the implementation driver.

VERDICT: FIXES_REQUIRED
