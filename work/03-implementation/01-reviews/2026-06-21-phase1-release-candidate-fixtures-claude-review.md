All checks pass. Provenance is verified directly against the 1.1 GB DILA archive — every claimed member path, `NUM`, `ETAT`, `DATE_DEBUT`, and `DATE_FIN` matches the fixture evidence exactly. Here is the review.

---

# Review — Phase 1.7 LEGI Release-Candidate Eval Fixtures

- **Date:** 2026-06-21
- **Reviewer:** Claude (Opus 4.8), automated code review
- **Scope:** Uncommitted diff in `/home/pierre/Work/jurisearch` adding the first Phase 1 LEGI release-*candidate* fixture set to `jurisearch-core::eval`, surfacing `release_candidates` through `jurisearch status` → `phase1_gate.eval_fixtures`, with schema, CLI-contract, and implementation-plan updates. Files: `crates/jurisearch-core/src/eval.rs`, `crates/jurisearch-core/src/schema.rs`, `crates/jurisearch-cli/tests/cli_contract.rs`, `work/03-implementation/IMPLEMENTATION_PLAN.md`.

## Verification performed

- **Gate safety (code-traced):** `phase1_gate_payload` (`main.rs:2576-2583`) keys the `release_gating_eval_fixtures` check solely off `eval_summary.release_gating > 0`. That field is still `0`; `release_candidates` is reported but feeds no check. The four new candidates therefore **cannot** flip `claim_allowed`, which remains `false`. Confirmed by the contract test still asserting `release_gating == 0` and `state == "not_ready"`.
- **Provenance (verified against the real archive):** I extracted all four cited members from `Freemium_legi_global_20250713-140000.tar.gz` and confirmed metadata matches the fixtures exactly:
  | Fixture | Member ID | NUM | ETAT | DATE_DEBUT | DATE_FIN |
  |---|---|---|---|---|---|
  | code-rural-r242-40 | `LEGIARTI000006590697` | R*242-40 | ABROGE | 1989-11-04 | 2003-08-07 |
  | veterinaire-deontologie | `LEGIARTI000006590698` | R*242-40 | MODIFIE | 2003-08-07 | 2003-10-11 |
  | reserve-naturelle-r242-41 | `LEGIARTI000006590700` | R*242-41 | ABROGE | 1989-11-04 | 2003-08-07 |
  | loi-1990-long-sejour | `LEGIARTI000006756700` | 27 | VIGUEUR | 2000-12-23 | 2999-01-01 |
  Every `expected_ids` `@<date>` suffix equals the source `DATE_DEBUT`. The `2999-01-01` sentinel claim is real.
- **Tests (re-run locally):** `cargo test -p jurisearch-core eval` → 6 passed; `cargo test -p jurisearch-cli --test cli_contract` → 36 passed, 0 failed. Matches Codex's reported runs.
- **Schema/struct/contract alignment:** `EvalFixtureSummary.release_candidates` (`eval.rs:60`) ↔ `schema.rs:362` ↔ `cli_contract.rs:162` are consistent. No stale `eval_fixtures.total == 2` assertions remain (the surviving `== 2` hits are `projection_coverage`/`embedding_coverage`, unrelated).

## Findings (ordered by severity)

### 1. Logic correctness of `is_release_candidate` — PASS (no issue)
`eval.rs:94-98` defines a candidate as `tier == ReleaseGating && is_source_verified() && !is_release_gating()`. This makes `release_candidates` and `release_gating` **mutually exclusive by construction**, so the summary can never double-count, and `source_verified (6) ≥ candidates (4) + gating (0) + source-verified dev fixtures (2)` holds. The added test branch (`eval.rs:358,363,368`) covers the tier/source/reviewer transitions correctly. Clean.

### 2. Low (fixture quality) — `veterinaire-deontologie` query is temporally ambiguous; the disambiguator lives only in prose
`eval.rs:250-253`: the query says *"…en 2003 ?"*, but `R*242-40` has two 2003 versions (`…598` valid 2003-08-07→2003-10-11, and a successor from 2003-10-11). The expected answer (`598`) is correct only under the `temporal_expectation`'s "2003-09-01" as-of, which is free text, not a machine-readable field. The same applies to the other candidates: unlike the hierarchy dev fixtures (which carry `HierarchyExpectation.as_of`), these set `hierarchy: None` and encode the as-of **only** in `temporal_expectation` prose. Non-blocking today (no execution harness exists yet, and these are candidates pending human review), but the eventual retrieval-execution harness will need a structured as-of, and this query's "en 2003" should be tightened during the named human review the plan calls for.

### 3. Low (semantics worth flagging to the human reviewer) — `R*242-40` carries two unrelated subject matters across recodification
The `code-rural-r242-40` fixture (natural-reserve contraventions, 1989) and `veterinaire-deontologie` fixture (administrative/political-responsibility prohibition, 2003) share article number `R*242-40` but assert different bodies/CONTEXTE. The metadata I extracted corroborates the version dates, but the *body/CONTEXTE* semantic claims (e.g. "code de déontologie vétérinaire") are exactly what only legal-domain review can confirm. This is correctly handled — these are `OfficialSourceChecked` candidates, not `HumanReviewed` gating fixtures — so it's an input to review, not a defect.

### 4. Info — minor naming/DRY nits, non-blocking
- `CITATION_STATE_STATUTORY_CATEGORY = "citation_state_statutory"` (`eval.rs:109`): the "state" token is a little opaque for a fixture that's really about citation links + sentinel-date normalization, but it's an internal label and consistent with the existing `*_statutory` convention.
- `PHASE1_SOURCE_ARCHIVE` (`eval.rs:111`) is used by the new `official_archive_evidence` helper, but the two pre-existing dev fixtures still inline the same archive literal in their `verified_against` strings (`eval.rs:187,217`). Folding those onto the constant would be tidier, but it's pre-existing and out of scope.

## Recommendations (all non-blocking)

1. During the named human review that promotes these candidates, tighten the `veterinaire-deontologie` query so the intended 2003 version is unambiguous (e.g. "au 1er septembre 2003"), aligning the query text with `temporal_expectation`.
2. When the retrieval-execution harness lands, give release-candidate fixtures a structured as-of field (or reuse a small typed temporal expectation) rather than relying on `temporal_expectation` prose, so the as-of is machine-checkable like the hierarchy fixtures' `as_of`.
3. Optionally migrate the two dev fixtures' `verified_against` strings onto the new `PHASE1_SOURCE_ARCHIVE` constant for consistency.

## Conclusion

The slice does exactly what it claims: it adds four source-checked LEGI fixtures whose provenance I independently verified against the real DILA archive, reports them under a new `release_candidates` field that is wired through struct → schema → CLI contract consistently, and — critically — keeps them **out** of the release gate (`release_gating` stays `0`, `claim_allowed` stays `false`). The fail-closed gate semantics are preserved, tests pass, and the implementation plan accurately records candidate status plus remaining human-review/benchmark work. The open items are fixture-quality refinements for the future human-review and execution-harness steps, not commit blockers.

**Verdict: GO**
