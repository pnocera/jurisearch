# Open Refactoring Questions

## Q1 — Decision-Part Fetch Boundary

**Pick: keep `fetch_payload` in `retrieval.rs`; keep decision-part/Judilibre-zone helpers under `enrichment/`.**

Deciding source facts:

- `fetch_payload` is generic fetch until the optional `--part` branch: it parses `DecisionPart`, opens the index, checks `QueryReadinessGate::Fetch`, calls `fetch_documents_json`, handles no-results, and only then calls `annotate_fetched_parts` when `part` is present ([main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:4056)).
- The part path is a separable overlay: `annotate_fetched_parts` walks fetched decision documents and delegates official-zone work to `official_decision_part` ([main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:4124)), while `official_decision_part` reads `decision_zones_json`, consults `zone_cache_action`, and may call `enrich_decision_from_judilibre` when `--online` is set ([main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:4221)).

Recommendation:

- Move `fetch_payload` and `emit_fetch` with retrieval as the plan says.
- Move `DecisionPart`, `annotate_fetched_parts`, `official_decision_part`, heuristic part extraction, and zone-cache helpers into `enrichment/decision_part.rs` / `enrichment/judilibre_zones.rs`.
- Let `retrieval::fetch_payload` call `enrichment::annotate_fetched_parts`.

Caveat: `DecisionPart::parse` is currently used directly by `fetch_payload` ([main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:4057)), so the enrichment module must expose a small public parsing surface. That is fine; it is much smaller than moving all fetch logic under enrichment.

## Q2 — France-LEGI `emit_eval` Special Case

**Pick: replace it with `emit_artifact`, but add one focused regression test first.**

Deciding source facts:

- The France-LEGI branch clones `args.out`, runs `eval_france_legi_payload`, pretty-serializes the `Value`, creates the parent directory, writes `format!("{rendered}\n")`, then calls `write_json(&response)` for stdout ([main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:1416)).
- `emit_artifact` does the same operations: pretty-serialize the same `Value`, create the parent directory, write `format!("{rendered}\n")`, then call `write_json(&response)` ([main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:1484)). `write_json` itself uses `serde_json::to_writer_pretty` and appends one newline ([main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:11968)).

So the consolidation is behavior-preserving for newline, pretty-printing, file-vs-stdout behavior, and object ordering because both paths serialize the same `serde_json::Value` with the same serde pretty serializer.

Test status:

- I found tests for schema coverage and France-LEGI artifact validity, but not a test that pins the `eval france-legi --out` writer branch specifically. The grep hits around `france_legi_artifact_keys_are_schema_documented` and artifact validators cover artifact shape, not the duplicated file/stdout emission path ([main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:12721)).

Recommendation:

- First extract or test a tiny common writer behavior: given a `Value` and `out`, the file contains `serde_json::to_string_pretty(value) + "\n"` and stdout remains the pretty JSON plus newline.
- Then replace the France-LEGI branch with `emit_artifact(response, out_path)`.

Caveat: an end-to-end CLI test for `eval france-legi --out` may need a real ready index, so a smaller unit test around a factored file-render helper is probably cheaper and more stable.

## Q3 — Split `crates/jurisearch-core/src/schema.rs`?

**Pick: split it, but later and with a golden equality test.**

Deciding source facts:

- It is hand-maintained, not generated: `compiled_schema()` is a literal `json!({ ... })` tree in normal Rust source ([schema.rs](/home/pierre/Work/jurisearch/crates/jurisearch-core/src/schema.rs:5)).
- It is frequently edited: `git log --since=2026-06-20 -- crates/jurisearch-core/src/schema.rs` shows 36 commits touching it, including CLI milestones, gates, eval artifacts, and zone retrieval.
- It already carries important contract invariants: `every_command_schema_name_resolves` verifies every command contract request/response schema name exists in `compiled_schema()` ([schema.rs](/home/pierre/Work/jurisearch/crates/jurisearch-core/src/schema.rs:1042)).

Recommendation:

- Do not split it during the first CLI `main.rs` extraction. It is secondary to the 13k-line CLI module.
- Do split it after the CLI move because it is hand-maintained and high-churn.

Suggested fragmentation:

- `schema/mod.rs`: `compiled_schema()`, root assembly, `exit_codes`, `error_object`, `session_envelope`, `common_enums`.
- `schema/search.rs`: search/compare/fetch/cite/context/related/expand schemas.
- `schema/admin.rs`: status/model/setup/doctor/stats/inspect/versions/diff/sync/help/serve/ingest schemas.
- `schema/eval.rs`: phase/eval/tune/France-LEGI/France-juris benchmark schemas.
- `schema/gates.rs`: Phase 1/Phase 2 gate and benchmark gate support schemas.

Preserve determinism by assembling a single `serde_json::Map` in an explicit order. Before splitting, capture the current `serde_json::to_string_pretty(&compiled_schema())` as a golden fixture and add a test that the split implementation is byte-identical to that fixture. Keep the existing command-name invariant too.

Caveat: `$ref` paths are global (`#/schemas/...`), so do not nest domain schemas under separate sub-objects unless you also rewrite every `$ref`. The split should be file-level only; the emitted JSON shape should stay unchanged.

## Q4 — Session Arg Structs Phase

**Pick: move session arg structs with `session.rs` in Phase 2, not with `args.rs` in Phase 1.**

Deciding source facts:

- The session structs are serde DTOs, not clap parsers: `SessionSearchArgs`, `SessionFetchArgs`, `SessionCiteArgs`, etc. derive `Deserialize`, use `#[serde(default = ...)]`, and have no `Args` derive or `#[arg(...)]` annotations ([main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:474)).
- They are consumed exclusively by session wrappers that deserialize from JSON `Value` and then construct the one-shot clap-style args: `session_search_payload` calls `serde_json::from_value::<SessionSearchArgs>` and builds `SearchArgs` ([main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5299)); `session_fetch_payload` does the same for `SessionFetchArgs` and `FetchArgs` ([main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5333)); dispatch routes JSONL commands to these wrappers ([main.rs](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:9392)).

Recommendation:

- Phase 1 `args.rs`: move clap-facing `Args`, `Subcommand`, and `ValueEnum` types only.
- Phase 2 `session.rs`: move `SessionRequest`, `SessionResponse`, all `Session*Args`, `dispatch_session_request`, `serve_jsonl`/`run_jsonl` session protocol pieces, and the `session_*_payload` wrappers.

Caveat: session DTOs reuse CLI enums and default functions (`CliKind`, `CliSearchMode`, `CliOutputFormat`, `CliGroupBy`, `CliZone`, `default_top_k`, etc.). Put those shared enums/defaults in `args.rs` or a small shared `types` section and import them into `session.rs`; do not move the DTOs themselves just to avoid imports.
