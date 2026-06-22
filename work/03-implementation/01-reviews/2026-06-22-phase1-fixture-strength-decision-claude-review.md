I've verified the diff against the referenced evidence and the actual gate/fixture code. Everything in the decision is grounded and the gate stays fail-closed. Here is the review.

## 1. Findings, ordered by severity

**None blocking.** The diff is documentation-only and every quantitative claim checks out against code and evidence.

**[Low — pre-existing, not introduced by this diff] Automated gate enforces *count*, not *discrimination*.**
`phase1_gate_payload` passes `release_gating_eval_fixtures` purely on `eval_summary.release_gating > 0` (`crates/jurisearch-cli/src/main.rs:3751-3758`). `is_release_gating()` (`crates/jurisearch-core/src/eval.rs:81-92`) only requires `tier == ReleaseGating`, source-verification, `HumanReviewed`/`HeldOut`, and a non-empty reviewer. So a reviewer could promote the *existing four saturated fixtures* — satisfying the automated gate — even though the decision file (lines 28, 30) explicitly says promotion must add *discriminating* cases that prove hybrid > BM25. The documented intent leads what the code enforces. The diff doesn't worsen this, but the new "Done" bullet records the decision as settled, so the gap is now load-bearing.

**[Low — consistency] "Release-gating" vs "release-candidate" wording drift in referenced docs.**
The decision file under review correctly says "release-candidate" throughout. But its cited evidence titles the table **"Release-Gating Fixture Results"** (`2026-06-22-phase1-eval-benchmark-summary.md:22-28`), and the pre-existing `IMPLEMENTATION_PLAN.md:728` (outside the diff) says "Release-gating fixture evidence: BM25 passed 4/4…". Code confirms `release_gating = 0` today (`is_release_candidate()` requires `!is_release_gating()`, eval.rs:96-100), so those four are candidates, not gating. The diff itself is the *correct* terminology; the upstream docs it points at are looser.

## 2. Open questions / residual risks

- The "keep hybrid as default over dense-only" inference (decision file line 22) rests on a 4-fixture set where dense scores 2/4. Directionally fine and appropriately hedged ("on this small set"), but it's a weak signal — BM25 and hybrid being tied at 4/4 means the set is equally blind to a *hybrid-vs-BM25* call.
- The concrete failing dev case `legi-hierarchy-temporal-sibling-2000` (verified at `eval.rs:199-221`, expects `legi:LEGIARTI000006850361@1999-03-28`, matching the benchmark summary line 69) is exactly the kind of discriminating, hierarchy-sensitive case the promotion requirements ask for — but the decision file describes harder distinctions generically (line 23) without naming this existing failure as a seed fixture. Minor missed link.

## 3. Recommendations (compatible, optional)

- When the discriminating fixtures land, either tighten the gate beyond `release_gating > 0` or attach a documented manual promotion checklist, so "named human review" can't pass the gate on saturated fixtures alone.
- Align the benchmark-summary table header to "Release-Candidate Fixture Results" to match both the code state and this decision file.
- Cite `legi-hierarchy-temporal-sibling-2000` explicitly as a candidate discriminating fixture in the promotion requirements.

## 4. Verification notes

- **Counts confirmed:** `phase1_release_candidate_fixtures()` returns exactly 4 (`eval.rs:122-129`); `phase1_hierarchy_dev_fixtures()` returns 2 (`eval.rs:115-120`); total 6. `selected_phase1_eval_fixtures(include_dev=false)` runs the 4 candidates only (`main.rs:622-628`). This matches "BM25 4/4, hybrid 4/4, dense 2/4" and "hybrid + dev 5/6".
- **Pass/fail figures** in the decision file match the benchmark summary table (BM25 4/0, dense 2/2, hybrid 4/0) and the include-dev run (5 pass / 1 fail).
- **Failing fixture identity** (`legi-hierarchy-temporal-sibling-2000` → `LEGIARTI000006850361@1999-03-28`) confirmed in source.
- **Fail-closed preserved:** `claim_allowed = checks.all(status == "pass")` (`main.rs:3778-3780`); `release_gating_eval_fixtures` is `pending` while `release_gating == 0`, so `claim_allowed = false`. The diff touches no code and does not flip any gate state — it converts the "decide if the set is strong enough" item from a Remaining blocker into a recorded Done assessment while keeping "promote only after named human legal-domain review" as the Remaining blocker. Reranker/status gates remain committed.
- **Diff scope:** confirmed 2 insertions / 1 deletion in `IMPLEMENTATION_PLAN.md` plus the new evidence file; no edits made by me.

VERDICT: GO
