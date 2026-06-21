# Review: Phase 1.3 `search --format concise|detailed` + detailed diagnostics

**Date:** 2026-06-21
**Reviewer:** Claude (Opus 4.8)
**Scope:** Uncommitted changes — CLI/session `format` arg, default-concise response, detailed diagnostics object, schema + CLI contract tests, IMPLEMENTATION_PLAN status. Ranking/retrieval must remain unchanged.
**Files:** `crates/jurisearch-cli/src/main.rs`, `crates/jurisearch-cli/tests/cli_contract.rs`, `crates/jurisearch-core/src/schema.rs`, `work/03-implementation/IMPLEMENTATION_PLAN.md`

**Build/test status:** `cargo build -p jurisearch-cli` OK · `cargo clippy -p jurisearch-cli` clean · `cargo test -p jurisearch-cli --test cli_contract` → 25 passed, 2 ignored (live-endpoint), 0 failed.

---

## Findings

### Correctness / retrieval invariance

- **Retrieval and ranking are provably unchanged.** The only retrieval-affecting inputs to `hybrid_candidates_json` are `query_text`, `query_embedding`, `embedding_fingerprint`, `retrieval_mode`, `as_of`, `kind_filter`, `lexical_limit`, `dense_limit`, `limit`. The diff merely hoists `kind_filter`, `lexical_limit`, and `dense_limit` from inline `HybridCandidateQuery` fields into identically-valued locals (`main.rs:503-509`) so diagnostics can read them. Values are byte-identical (`Some("article")` for `Code` else `None`; `top_k * 4`). `format`/`diagnostics` are attached to the response JSON *after* retrieval. No ranking path touched. ✅
- **No key collision.** `format` and `diagnostics` are new top-level keys not produced by the storage layer (which emits `query`, `retrieval_mode`, `as_of`, `limit`, `candidates`). Safe to always attach. ✅
- **`format` is now emitted on every search response** (concise included), and `SearchResponse` declares no `required` set, so the always-present field is schema-valid. ✅
- **Diagnostics null-handling is correct.** `lexical_query_text` is `None` for non-lexical (dense-only) mode, and `embedding_fingerprint` is `None` when dense is unused — both verified by the new detailed test (`uses_dense=false`, fingerprint null). The dense-only branch sets `query_text = args.query.trim()` (not parade-normalized); reporting it as null in that mode is the right call since lexical text is meaningless there. ✅
- **Session path threads `format` correctly.** `SessionSearchArgs.format` (`#[serde(default = "default_output_format")]` → concise) is forwarded into `SearchArgs` in `session_search_payload` (`main.rs:670`). ✅
- **Plan status update is accurate** — adds a "Done" line for `--format` and removes it from "Remaining"; no overstatement.

### Minor / low severity

1. **Concise test does not assert diagnostics absence.** The concise assertion block (`cli_contract.rs:560`) checks `format == "concise"` but never asserts `json["diagnostics"].is_null()`. A regression that leaked the diagnostics object into concise output would pass undetected. The detailed branch is well covered; the concise branch is the one missing the negative assertion.
2. **`OutputFormat` core enum has no `as_str()`.** `contract.rs` adds `OutputFormat { Concise, Detailed }`, but the string mapping is duplicated as a CLI-local `output_format_name()` helper (`main.rs:2166`), unlike the sibling `RetrievalMode::as_str()` / `LegalKind::canonical_result_kind()` which live on the core type. Minor inconsistency / missed reuse — an `OutputFormat::as_str()` on the contract type would be the natural home.
3. **`diagnostics.retrieval` is an opaque object in the schema.** `schema.rs:133` types it as `{ "type": "object" }` with no nested properties, while the runtime object exposes 7 fields (`mode`, `uses_lexical`, `uses_dense`, `lexical_limit`, `dense_limit`, `embedding_fingerprint`, `kind_filter`). Agents consuming `help schema --json` get no contract for those sub-fields. Documenting them would match the precedent set by the fully-specified `pagination` block right above it.
4. **Enum value drift surface.** `["concise","detailed"]` now appears in four code spots (`OutputFormat`, `CliOutputFormat`, the `schema.rs` literal, `output_format_name`) plus the pre-existing `common_enums.response_format`. This mirrors the existing `mode`/`kind` duplication pattern, so it's consistent with the codebase — noted only as drift risk if a third format is ever added.
5. **No session-mode test for `format`.** Only the one-shot CLI path exercises `--format detailed`. The session path is a thin forward, so risk is low, but a JSONL test would close the loop symmetrically with how `expand`/`search` session paths are otherwise covered.

### Positive notes

- This change fulfills the previously-advertised-but-unimplemented `common_enums.response_format` contract enum (`schema.rs:75`) — good follow-through rather than introducing a new surface.
- Contract type, JSON schema (request default + response enum), CLI arg, and session arg are all updated coherently in one pass.
- The local-variable refactor is the minimal, clean way to share the limits/filter between the query and diagnostics without altering behavior.

---

## Recommendations

- (Should) Add `assert!(json["diagnostics"].is_null())` to the concise assertion block so the concise/detailed boundary is enforced both ways. (Finding 1)
- (Nice) Move the format→string mapping onto `OutputFormat::as_str()` in `contract.rs` and drop the CLI-local `output_format_name`. (Finding 2)
- (Nice) Spell out the `diagnostics.retrieval` sub-properties in the schema to match the `pagination` block's specificity. (Finding 3)
- (Optional) Add a JSONL session test asserting `format` passthrough. (Finding 5)

None of these block the change: retrieval is invariant, the contract is coherent, and tests/clippy are green. The findings are test-completeness and schema-documentation polish.

Verdict: GO
