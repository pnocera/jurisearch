# Code Review: CLI Enhancement Analysis

## Findings

### HIGH: Warm-session parity is overstated and partly wrong

The analysis correctly identifies that `related`, `ingest`, generic `eval`, and `sync` are not available through the JSONL session path, but it also states that `model fetch` and `setup` are absent from the warm protocol. That is inaccurate: `dispatch_session_request` handles `"model fetch"`, `"eval phase1"`, and `"setup"` directly in `crates/jurisearch-cli/src/main.rs:4222`. The roadmap then treats these as examples of session parity gaps, which can send implementation work toward already-covered capabilities while missing the more precise gaps.

Recommendation: revise the inventory and Section B to split the session surface into:

- implemented in session: `help`, `help schema`, `status`, `search`, `fetch`, `cite`, `context`, `expand`, `model fetch`, `eval phase1`, `setup`;
- missing/stubbed in session: `related`, `ingest` subcommands, `eval france-legi`, future generic `eval run`, `sync`;
- one-shot-only by design, if any, with explicit reasons in `help schema`.

### HIGH: The analysis misses a concrete self-description gap for `eval france-legi`

The CLI has a real one-shot `eval france-legi` implementation in `EvalSubcommand::FranceLegi` and `eval_france_legi_payload` (`crates/jurisearch-cli/src/main.rs:326` and `crates/jurisearch-cli/src/main.rs:593`), but the compiled command contract only lists `eval phase1`, and `compiled_schema()` has `EvalPhase1Request`/`EvalPhase1Response` but no `EvalFranceLegiRequest` or response schema. This is exactly the kind of "available but not fully self-describing" problem the document is supposed to surface, yet it is not called out as a P0 issue.

Recommendation: add a specific P0 item to register `eval france-legi` in `COMMANDS`, add its request/response schema, and decide whether it should be session-callable or explicitly one-shot-only. This is more actionable than the broader "complete schemas for every command" statement because it points to a currently implemented command missing from the agent contract.

### MEDIUM: `fetch --as-of` and `fetch --part` are described as implemented, but the code rejects them

The inventory table lists `fetch` as implemented with `--as-of --part`. In the live code, `FetchArgs` accepts those flags syntactically, but `fetch_payload()` immediately returns `bad_input` when either is present: "fetch --as-of and --part are reserved for a later fetch slice and are not applied yet" (`crates/jurisearch-cli/src/main.rs:1511`). That means the current table overstates temporal/sliced fetch capability and hides a user-visible reserved-flag gap.

Recommendation: mark `fetch` as "implemented, except reserved `--as-of`/`--part` flags" and add an explicit gap for implementing or removing those flags. If the roadmap keeps temporal `versions`/`diff`, it should also say whether `fetch --as-of` remains the primitive for version-pinned document retrieval.

### MEDIUM: Schema/output-contract criticism is too broad and should be narrowed

Section K says the error taxonomy is not expressed as structured error-code enums and that `routing`/`diagnostics`/`pagination` sub-schemas are undocumented. The current schema already includes `exit_codes`, an `error_object.code` enum, `SearchResponse.pagination`, and `SearchResponse.diagnostics` in `crates/jurisearch-core/src/schema.rs`. The real gap is narrower: `SearchResponse.routing` is emitted by `search_with_postgres()` but is absent from `compiled_schema()`, per-flag metadata is shallow, `RelatedRequest`/`RelatedResponse` are named in the contract but not schema-defined, and `eval france-legi` is absent as noted above.

Recommendation: rewrite Section K around the concrete missing schema fields and commands:

- add `SearchResponse.routing` with `query_type`, `chosen_backend`, `candidate_count`, and `fallback_path`;
- define placeholder or real schemas for `RelatedRequest`/`RelatedResponse` and `SyncRequest`/`SyncResponse`;
- add `EvalFranceLegiRequest`/`EvalFranceLegiResponse`;
- keep the per-flag schema and `help <command> --json` recommendations;
- remove the claim that error-code enums and diagnostics/pagination are not represented at all.

### MEDIUM: Dependency-health gap ignores partial coverage already present in `status`

Section I says there is no `doctor`/`health` to verify dependencies and specifically says embedding endpoint health is tribal knowledge. The CLI does not have a full `doctor` or explicit `db` lifecycle command, so the broader gap is valid. However, `status` already reports embedding configuration and `EmbeddingEndpointStatus`, and `embedding_endpoint_status_json()` probes local loopback embedding endpoints with a TCP connection (`crates/jurisearch-cli/src/main.rs:4050`). The analysis should not imply that endpoint reachability is completely absent.

Recommendation: reframe Section I as "status has partial embedding health; missing is a comprehensive doctor/preflight plus explicit DB lifecycle." Then list the remaining checks: PG data-dir status without opening/owning it, migrations, extension assets, endpoint model compatibility, model cache, and replay/index readiness.

### LOW: The roadmap should distinguish "analysis-session evidence" from durable product requirements

The "Evidence" section is useful, but several rows are based on this session's bespoke benchmark workflow. Some of those are clearly product-level CLI needs (`group-by document`, pooling, metrics, stats), while others are implementation choices (`external LLM judge hook`, `sql --read-only`) that need threat-model and contract decisions before becoming roadmap items.

Recommendation: keep the evidence table, but add a short "requirement strength" column or separate "must own" from "escape hatch / optional" capabilities. For example, document-level grouping and eval metrics are core; a generic SQL escape hatch should remain explicitly non-goal or gated because it undercuts the "single typed interface" principle.

## Summary

The analysis has the right overall direction: the CLI is strong for single-shot retrieval and verification, while graph traversal, evaluation generality, result shaping, tuning, introspection, and lifecycle controls remain real gaps. The document needs fixes before it should drive implementation, mainly because it misstates the current warm-session surface, overlooks the already-implemented but unschema'd `eval france-legi`, and over-broadens some schema and health criticisms.

VERDICT: FIXES_REQUIRED
