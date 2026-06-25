I have everything I need. Writing the review.

# Claude Review - Authority-Aware Ranking Implementation Plan

## Findings

No BLOCKERs. The plan is source-grounded, faithfully implements the r4-GO'd design, preserves the controlling invariant, and its phases are independently reviewable. It also correctly catches one design over-reach (the eval-tune authority sweep). The items below are clarifications to land before the corresponding phase gates.

### WARN 1 — A1: the `effective_authority_weight` contract omits the `≤ 0.0 → None` normalization, which is the load-bearing inertness primitive

- **Where:** A1 (`...-implementation-plan.md:61`), used by A3 (`:142-143`) and A4 (`:172-176`).
- **Issue:** A1 lists `effective_authority_weight(options: &RetrievalOptions) -> Option<f64>` with no statement that it filters `≤ 0.0` (and non-finite) to `None`. A3 then derives `rerank_on = effective_authority_weight(&args.retrieval_options()).is_some()`. That `.is_some()` is only correct if the helper already normalizes `0.0 → None` (design §4.4/D8: `authority_weight.filter(|w| *w > 0.0)`). If an implementer writes a passthrough, `--authority-weight 0.0` takes the ON path and breaks the byte-identical invariant the whole plan is built on. The A5 `0.0 == unset` test would catch it, but the most invariant-critical primitive in the build doc should not be left implicit.
- **Recommend:** In A1 state that `effective_authority_weight` returns `None` when the field is unset, non-finite, or `≤ 0.0`; add an A1 unit test asserting `effective_authority_weight(Some(0.0)) == None` and `effective_authority_weight(None) == None`, so `rerank_on = ….is_some()` is provably the ON test.

### WARN 2 — A4: `authority_rerank` is wired to run unconditionally after parse; it must be scoped to the hybrid candidate path

- **Where:** A4 task ("after JSON parse and before truncation, call `authority_rerank` on `response["candidates"]`", `:180`) vs A4 acceptance ("Structured citation results remain unaffected", `:207`).
- **Issue:** In `search_with_postgres` a citation-shaped query in Hybrid mode can route to `resolve_legi_citation_json` (`main.rs:3742-3766`, `chosen_backend="structured_citation"`), whose candidates are not RRF-ranked decisions and use `limit: args.top_k` (not the widened limit). Calling `authority_rerank` unconditionally contradicts the acceptance line. In practice `--kind decision` makes the structured path return 0 and fall back to hybrid, so impact is low — but the task instruction is imprecise and a literal implementation could reorder a structured set.
- **Recommend:** A4 should gate the rerank call (and the forced `next_cursor=null` / `cursor_supported=false`) to the hybrid branch only, e.g. `chosen_backend != "structured_citation"`, and state explicitly that structured-citation responses bypass authority entirely.

### WARN 3 — A5/A6: the design §7.1 mandatory recall regression guard is reorganized into the measured-only benchmark, but the plan never states the Phase 2 gate command stays knob-free

- **Where:** A6 ("recall@10 OFF vs ON …", `:288`), A5 guard tests (`:237-240`), vs design §7.1 (which said run `eval france-juris` / `eval france-juris-zones` — the gate commands — with `--authority-weight` set/unset).
- **Issue:** The plan moves the recall@10 OFF-vs-ON comparison inside the new `eval france-juris-authority` benchmark and (via A5) keeps the gate artifact unchanged. That is the safer choice, but the plan never explicitly says the gate command `eval france-juris` does **not** receive `--authority-weight`. An implementer following design §7.1 literally could add the knob to the gate command and risk contaminating `phase2_full_juridic_corpus` gate inputs — exactly the failure the review wants to prevent. It also leaves unstated that the benchmark's recall@10 must use the same gold recipe as the gate so the OFF/ON comparison is apples-to-apples against the 0.50 floor.
- **Recommend:** State explicitly that (a) the Phase 2 gate command `eval france-juris` does not gain the authority knob; (b) the §7.1 regression guard is realized inside the measured-only benchmark, recomputing recall@10 with the gate's gold recipe; (c) this supersedes design §7.1's "run the gate command with the knob."

### NIT 1 — A6: "OFF widened window" is self-contradictory; specify how the benchmark window is produced

- **Where:** A6 ("Build pairwise authority-lift data from OFF widened windows", `:277-282`).
- **Issue:** The user OFF path uses `query_limit = top_k+1` and does not widen; the widened window only exists when `project_authority` is on. The benchmark therefore needs its own widened, authority-projected, **un-reranked** fetch, from which it computes both `authority_lift_off` (natural order) and `authority_lift_on` (after `authority_rerank`). As written, "OFF widened window" reads as a contradiction.
- **Recommend:** Spell out that the benchmark issues a widened, `project_authority=true`, un-reranked fetch and derives both the pair set and both lift numbers from that single window.

### NIT 2 — §0/A6 supersede design §7.3 (eval-tune authority sweep); flag the divergence

- **Where:** §0 ("Do not attach authority tuning to that path…", `:26-28`) and A6 ("…not to generic article-qrel `eval tune`", `:291-294`) vs design §7.3.
- **Issue:** The plan correctly refuses to attach authority tuning to generic `eval tune` (confirmed: that path runs `kind_filter: Some("article")`, `main.rs:1869`, so authority would be inert against statutes). This is the right call but directly contradicts the GO'd design §7.3, which a reviewer cross-checking plan-vs-design will read as an omission.
- **Recommend:** Add one line that this intentionally supersedes design §7.3, with the article-qrel reason.

### NIT 3 — A5: elevate the byte-identical default check from "manual or scripted fixture comparison" to a committed automated test

- **Where:** A5 acceptance ("A manual or scripted fixture comparison shows unset and explicit `0.0` are identical…", `:254-255`).
- **Issue:** Design §6.2 calls the byte-identical golden diff "the single most important test." A "manual or scripted" comparison is weaker than a CI-enforced golden/contract test.
- **Recommend:** Require a committed automated golden/contract test (CLI + session) asserting unset ≡ `0.0` and default-output stability, so the invariant is enforced in CI.

## Open Questions / Residual Risks

- **Benchmark/gate recall parity (residual).** For A6's "recall@10 does not regress below … the Phase 2 floor" to be meaningful, the new benchmark's recall@10 must be computed with the same gold/qrel recipe and grouping the Phase 2 gate uses. If it drifts, the "no regression" claim is not apples-to-apples with the 0.50 floor. (Tracks WARN 3.)
- **Tie-break id per grouping (low risk).** A1 says "preserve deterministic fallback order by existing id when adjusted scores tie," but the SQL secondary sort is `chunk_id` for chunk grouping and `document_id` for document/zone (`retrieval.rs:318/370`, `zone_retrieval.rs:249`). A strictly stable sort over the already-SQL-ordered window inherits the correct order on ties regardless of which id is read; if a non-stable sort with an explicit id key is used, the id must be chosen per grouping. Worth one sentence binding the helper to a stable sort.

## Verification Notes

Inspected (read, not run):
- `retrieval.rs`: `RetrievalOptions` (63-68), `HybridCandidateQuery` (70-86), `DecisionFilters::predicate` (120-156), shared `effective_rrf_weights`/`effective_probes` (161-182), `hybrid_candidates_json` chunk `limited` SELECT + candidate JSON (305-342) and document `scored`/`best_document_chunks`/`limited` + candidate JSON (343-396).
- `zone_retrieval.rs`: `zone_candidates_json` incl. `scored` SELECT (228-239) and candidate JSON (260-270).
- `main.rs`: `SearchArgs` (296-350), `retrieval_options()` (398-405), `validate_retrieval_options` (418-439), `search_pagination_value` (3258-3280, has `cursor_note`), `search_payload` (3282-3324, validate→zone-route ordering), `zone_search_payload` (3373-3491: `--kind code` rejection 3379, `top_k*20` pool, `top_k+1` limit, truncate 3468-3475, hardcoded `cursor_supported=true`), `search_with_postgres` (3659-3834: `pool_multiplier` 4/20, `query_limit=top_k+1`, truncate/next_cursor 3777-3785, `cursor_supported` 3789, diagnostics), `SessionSearchArgs` (474-507, `#[serde(default)]` fields incl. `zone`), `session_search_payload` (5242-5274, field-by-field rebuild), eval-tune pool `kind_filter: Some("article")` (1869), `zone_benchmark_artifact` (`state:"measured"`, `gate_input:false`, 3013-3055). Confirmed symbol locations for `parse_search_cursor` (11723), `eval_tune_payload` (2063), `eval_france_juris_payload` (2526), `eval_france_juris_zones_payload` (2834). Read prior design reviews r1, r3, r4.

Did NOT run: cargo build/test, any eval subcommand, or any SQL. Did not enumerate every `HybridCandidateQuery`/`ZoneCandidateQuery`/`RetrievalOptions` construction site (relied on the compiler-forced-update property of adding a non-`Option`/`bool` field), did not read `france_juris.rs` gold builders in full, and did not read the full gate re-derivation (`main.rs:10712-10797`).

Cross-cut confirmation against the review's focus areas: unset/`0.0` inertness is correct in intent but under-specified at one helper (WARN 1); gated projection of `publication` is well-specified and compiler-protected for OFF identity; main/zone consistency via one shared helper is enforced; cursor truthfulness (first-page-only, no legacy cursor on ON, parser untouched) is correct; kind gating is correct including the main-rejects-`all` / zone-allows-`all` asymmetry; eval-tune exclusion is correct and verified; the authority benchmark is measured-only and separate-kind (cannot touch the gate); no schema migration is required for v1; and the test set is strong, with the noted clarifications.

## Verdict

VERDICT: GO
