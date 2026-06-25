# SOLID / DRY Review: `work/06-refactoring/refactoring-plan.md`

## BLOCKER

No BLOCKER findings.

The proposed split is directionally sound for a behavior-preserving first refactor: it attacks the real monolith, keeps the first move inside the binary crate, separates cross-cutting helpers before command modules move, and explicitly prevents sibling command modules from importing each other just to reach helper code. I do not see a SOLID/DRY issue severe enough to make the plan unsafe to execute as a first mechanical decomposition.

## WARN

### WARN 1: The plan preserves too much command-surface triplication

The plan moves the current command surfaces into modules, but it mostly keeps the same OCP pressure points: a new command or new argument still requires coordinated edits in the clap enum/args, `dispatch::run`, session DTOs/wrappers, `dispatch_session_request`, `jurisearch-core::contract::COMMANDS`, `SESSION_EXCLUDED_COMMANDS`, and `compiled_schema()`. The current source shows the spread clearly: clap command registration is in `Command` (`crates/jurisearch-cli/src/main.rs:186`), one-shot dispatch in `run` (`main.rs:1231`), session string dispatch in `dispatch_session_request` (`main.rs:9398`), contract metadata in `COMMANDS` (`crates/jurisearch-core/src/contract.rs:66`), session exclusions in `SESSION_EXCLUDED_COMMANDS` (`contract.rs:255`), and schema bodies in `compiled_schema()` (`crates/jurisearch-core/src/schema.rs:5`). Splitting files reduces merge conflicts, but it does not yet make extension localized.

Recommendation: add a post-mechanical step to the plan that introduces a single internal command registry or descriptor table for the non-clap metadata: command name, session availability, request/response schema names, and handler entrypoint where practical. Keep clap derive types if desired, but make `agent_help`, session exclusion checks, and schema command listing derive from the same command inventory instead of separate literal lists. This can stay simple and not become a broad command-handler trait.

### WARN 2: Session DTO to CLI args rebuilding remains the biggest DRY hole

The plan deliberately keeps `Session*Args` in `session.rs` and has session wrappers rebuild clap args structs before calling payloads. That is source-accurate, but it preserves a duplication pattern that is already wide. For example, `SessionSearchArgs` duplicates nearly every field in `SearchArgs` (`main.rs:295`, `main.rs:473`) and `session_search_payload` manually rebuilds `SearchArgs` field by field (`main.rs:5309`). The same pattern exists for fetch/cite/context/related/compare (`main.rs:5333`, `main.rs:5352`, `main.rs:5370`, `main.rs:5389`, `main.rs:5407`). Adding one search option now means touching clap args, session DTOs, conversion code, schema, and sometimes one-shot validation.

Recommendation: amend Phase 2/3 to introduce shared internal request structs for command payloads, e.g. `SearchRequest`, `FetchRequest`, `CiteRequest`, that derive `Deserialize` where useful and are produced by `TryFrom<SearchArgs>` for one-shot clap and by session deserialization directly. Payload functions should accept the shared request type, not clap structs. This avoids command-handler traits while collapsing the parallel `*_payload` / `session_*_payload` argument-rebuild surface.

### WARN 3: `retrieval.rs` is likely to become a second command monolith

The plan assigns search, zone search, fetch, cite, context, related, compare, and expand to one `retrieval.rs`. That is coherent as a top-level domain, but the current bodies are not one responsibility. `search_payload` plus `zone_search_payload` own query normalization, readiness, pagination, diagnostics, and two storage paths (`main.rs:3282`, `main.rs:3373`). `fetch_payload` additionally crosses into decision-part enrichment. `cite_payload` owns citation state and optional online confirmation. `related_payload` and `context_payload` are graph/structure readers rather than retrieval-ranking code. A single flat `retrieval.rs` will still force unrelated search, citation, fetch, and graph changes into one large file.

Recommendation: keep the public module name `retrieval`, but make Phase 3 create `retrieval/` submodules from the start or immediately after the first move: `search.rs`, `zone.rs`, `fetch.rs`, `cite.rs`, `context.rs`, `related.rs`, `compare.rs`, and `expand.rs`, re-exporting narrow payload functions from `retrieval/mod.rs`. This preserves the plan's dependency direction while giving each leaf a clearer SRP boundary.

### WARN 4: `ingest.rs` keeps LEGI/JURI archive orchestration duplication alive

The plan says to move `ingest_legi_archives_payload`, `ingest_juri_archives_payload`, archive member processing, quarantine helpers, embed chunks, backfill, and the zone-unit pipeline into one `ingest.rs`. That is a useful extraction from `main.rs`, but it does not directly address the clearest ingest DRY issue: the LEGI and JURI archive payloads have parallel run setup, planning, batching, fatal-error handling, manifest update, terminal status, replay snapshot refresh, and response shaping (`main.rs:5843`, `main.rs:6209`). The source-specific member parsing differs; the outer archive-run lifecycle is duplicated.

Recommendation: add an ingest-internal abstraction after the mechanical move: an `ArchiveIngestRun` or small generic runner that owns the common lifecycle (plan, start run, select archives, read members, flush batches, finalize manifest/status, replay snapshot) and delegates source-specific manifest/counter/member processing through function parameters or a tiny source adapter. Keep it private to `ingest` and do not move it into `jurisearch-ingest` yet.

### WARN 5: Phase gate extraction should factor common artifact-loading and validation helpers

Splitting `gates/phase1.rs` and `gates/phase2.rs` is correct for SRP, but the plan treats them mostly as moved code. The current gate code repeats a pattern that deserves a shared private helper: read an env-configured artifact path, parse JSON, normalize diagnostics, run validator, set `state`/`artifact_error`, and expose evidence/metrics/categories (`main.rs:10024`, `main.rs:10688`). It also has generic dotted JSON pointer helpers (`main.rs:10283`) and category validation shapes used by multiple validators.

Recommendation: add `gates/artifact.rs` or `gates/support.rs` to Phase 4 for common artifact loading, pointer helpers, and validator result shaping. Keep policy constants and phase-specific floor logic in `phase1.rs` and `phase2.rs`, but do not duplicate the mechanics of loading and reporting benchmark artifacts as more gates are added.

### WARN 6: The plan leaves eval benchmark families flatter than they should be

The plan moves all eval code into `eval.rs`, including `eval run`, `eval tune`, France-LEGI, France-juris, and France-juris-zones. The current source shows these are distinct responsibilities: generic eval runner/tuner (`main.rs:1810`, `main.rs:2063`), LEGI official benchmark (`main.rs:2170`), full jurisprudence benchmark (`main.rs:2732`), and advisory zone benchmark (`main.rs:2834`). The France-juris and zone paths have obvious near-parallel category/search/artifact construction but different storage queries and claims (`main.rs:2683`, `main.rs:2900`, `main.rs:3019`). A flat `eval.rs` will likely become the next review hotspot.

Recommendation: make Phase 3/4 or a follow-up commit split `eval/` into `generic.rs`, `france_legi.rs`, `france_juris.rs`, `zones.rs`, and `artifact.rs`/`metrics.rs`. Extract only genuinely shared metric/category/artifact helpers; keep claim-specific validation and provenance text in the relevant benchmark module.

## NIT

### NIT 1: Resolve the `errors.rs` versus `output.rs` ambiguity in the plan

The plan says the shared `ErrorObject` constructors and mappings should live in `errors.rs`, but notes that `output.rs` would also be acceptable. Leaving both options in the plan weakens the module contract: `output.rs` should own serialization/emission (`write_json`, `emit_error`, session response writing), while `errors.rs` should own construction and mapping (`dependency_unavailable`, `storage_error_object`, `embedding_error_object`, etc.).

Recommendation: update the plan to choose `errors.rs` definitively and have `output.rs` depend only on `ErrorObject`/`ProcessExit` for emission. Do not leave this decision to the mechanical move.

### NIT 2: Include private helper dependencies explicitly in leaf moves

The shared leaf list correctly calls out most private dependencies, but the `date.rs` move lists `today_utc` and `unix_seconds` without explicitly listing `civil_from_days`, which `today_utc` calls (`main.rs:11925`, `main.rs:11931`). This is minor, but the plan is intended to guide mechanical moved-symbol diffs, so omitted private dependencies create avoidable churn.

Recommendation: add `civil_from_days` to `date.rs`, and in general state that each listed helper move includes its private helper closure unless separately assigned.

### NIT 3: Keep `sync` ownership explicit

The plan places `sync_payload` under ingest if it remains a source-ingest helper. That is reasonable because the current implementation is a thin incremental wrapper over `ingest_legi_archives_payload` / `ingest_juri_archives_payload` (`main.rs:1341`), but it is easy for future sync behavior to grow beyond archive ingestion.

Recommendation: state that `sync_payload` may live in `ingest.rs` only while it remains archive-ingest orchestration; if it gains independent delta/history semantics, split it into `sync.rs` rather than growing `ingest.rs`.

VERDICT: GO
