# Q&A — 20260624-111150

## Question

# Decision: should we build authority-aware ranking, and at what depth?

Re your earlier design in `qa/20260624-104744-design-a-authority-aware-ranking-b-judil.md` (re-read it for context). I need your DECISIVE engineering recommendation on the AUTHORITY RANKING scope specifically — not a re-design.

## Situation
- The unified statutes+jurisprudence index already PASSES the Phase 2 gate (judicial recall@10 1.0, administrative 0.74, citation accuracy 1.0).
- Your authority design is deep surgery on the core hybrid retrieval SQL (`hybrid_candidates_json`: chunk + document grouping, both cursor predicates, a cursor-format version bump), touching the production search path used by everything.
- You yourself noted: known-item recall@10 will NOT move with authority weighting, so it needs a NEW advisory `eval authority-ranking` harness (graph-edge decision→statute gold, graded nDCG@10) just to tell whether it helps, and the honest default is 0.0 unless tuning proves a gain.
- Separately, a plain RRF dense-weight sweep is being measured now to lift administrative retrieval.

## The question
Which is the right call, and WHY:
- (A) Build the safe slice only: the tunable `decision_authority_weight` hook (default 0.0) + non-regression tests (articles unchanged, no pagination dup/skip) + the advisory graph-gold eval — available and tunable, but never degrades ranking by default.
- (B) Build it fully and ship a NON-ZERO tuned default — accepting the core-retrieval risk and committing to the eval work needed to justify the default.
- (C) Skip / defer authority ranking entirely — on the view that RRF tuning + zone enrichment deliver the ranking/quality value more cheaply and at far lower risk to the core path.

Consider concretely: (1) the real-world value of a publication-authority boost for THIS corpus given candidate generation is unchanged; (2) whether the graph-edge gold can even produce a DEFENSIBLE non-zero default (enough decisions cite statutes with authority-graded relevance?), or whether such a default would be over-fit/arbitrary; (3) whether RRF tuning already covers the practical ranking-quality need; (4) the maintenance/contract cost of a cursor version bump on the core search path.

Static reasoning + source is fine; do NOT open the live index `/mnt/models/jurisearch-index/phase2-full-juridic` (a sweep holds it) — use the backup `...backup-20260624` only if you must. Give a clear recommendation (A/B/C) with reasoning.

## Answer

# Decision: authority-aware ranking scope

## Recommendation: C — defer authority-aware ranking

Do **not** build authority-aware ranking now, not even the “safe” default-0 hook.

The right current allocation is:

1. Finish the RRF dense-weight sweep and adopt the best evidence-backed default if it improves the Phase 2 retrieval profile.
2. Build Judilibre lazy zone enrichment, because it adds a distinct capability users can see and trust.
3. Revisit authority ranking only if a real product requirement appears, e.g. “prefer published decisions among otherwise relevant jurisprudence results”, and start with an eval-only study before touching production retrieval.

## Why C, not A or B

### 1. The value is real but secondary

A publication-authority boost is a presentation preference, not a relevance model.

For this corpus, the core search problem is still: find the right statute/decision using BM25 + dense retrieval over millions of chunks. Authority weighting cannot retrieve a missing decision; it only reorders candidates already found by RRF. That makes it useful only in the narrow case where:

- multiple decisions are already in the candidate pool;
- several are similarly relevant;
- publication status is a better ordering criterion than textual match.

That is a valid legal-search UX preference, but it is not a Phase 2 correctness blocker. The index already passes the benchmark, and RRF tuning attacks the actual retrieval objective more directly.

### 2. A defensible non-zero default is unlikely from the proposed graph gold

The proposed authority eval would label:

- `2` = decision cites the article and is published/high-authority;
- `1` = decision cites the article but is not published/high-authority.

That can measure whether the boost does what it says: move published citing decisions up. It does **not** prove the published decision is more responsive to a user’s query than an unpublished decision citing the same article.

So the graph-gold eval can justify an **advisory knob**, but it is weak evidence for a global non-zero production default. It risks validating the ranking term against labels that were partly defined by the same authority feature being tuned.

That makes option B too aggressive.

### 3. Option A still touches too much core machinery for an unproven feature

Even with `decision_authority_weight=0.0`, the safe slice is not free:

- it changes `hybrid_candidates_json()` in both chunk and document grouping;
- it adds new scoring fields and likely new final-score ordering;
- it affects cursor semantics and may require a cursor-format version bump;
- it needs pagination no-dup/no-skip tests;
- it expands `RetrievalOptions`, CLI args, session args, tune plumbing, and docs.

The default-0 hook avoids ranking degradation, but it still increases maintenance surface in the production search path used by everything. That is not a good trade while the expected default remains zero.

### 4. RRF tuning already covers the current ranking-quality need

The live tuning sweep is the better lever:

- it changes existing retrieval weights, not retrieval semantics;
- it is already supported by `rrf_weights()` / request-scoped `RetrievalOptions`;
- it is evaluated by the existing France-juris benchmark;
- it can improve administrative recall directly, which authority ranking likely cannot.

Authority ranking should not compete with that work unless there is evidence RRF tuning leaves a specific “published decisions are buried under equally relevant unpublished decisions” problem.

### 5. Cursor contract churn is not worth it right now

The cursor-format issue is the clearest engineering smell. A relevance preference with unproven benefit should not force versioning or subtle pagination changes in the main retrieval SQL.

If authority ranking is later built, it should be done when the team is willing to own:

- final-score cursors;
- migration/compat notes for session clients;
- test fixtures that cover chunk and document grouping;
- eval evidence that justifies enabling the feature.

That bar is higher than the current need.

## What to do instead

### Now

- Defer authority ranking implementation entirely.
- Do not add `decision_authority_weight`.
- Do not change `hybrid_candidates_json()` or cursor formats.
- Complete RRF tuning and zone enrichment first.

### Cheap non-invasive compromise

If you want to preserve the idea without production risk, add only a planning note or issue:

- “Authority ranking candidate: evaluate published-decision preference over graph-derived decision-to-statute pools before retrieval SQL changes.”

Do not build the eval harness yet unless you have a concrete tuning decision to make.

### Later trigger to revisit

Reopen authority ranking only when one of these is true:

- user-facing review shows published/recueil decisions are consistently below less authoritative but similarly relevant decisions;
- a legal-domain reviewer explicitly wants authority ordering as a product behavior;
- a graph-gold or judged-pool eval shows a meaningful ordering gain with no France-juris recall regression;
- the retrieval SQL is already being revised for another reason, making cursor/version work less marginal.

## If revisited later

The correct later shape would be A, not B:

- build the hook behind default `0.0`;
- add the advisory eval first or in the same PR;
- tune only on backup/staging indexes;
- keep the non-zero default out of production until a human-reviewed artifact supports it.

But that is a later feature, not a current Phase 2 enhancement.

## Bottom line

Choose **C** now. Authority-aware ranking is plausible but not urgent, not gate-relevant, not strongly justifiable from the proposed automatic labels, and too invasive relative to its expected value. Spend the engineering budget on RRF tuning and Judilibre zone enrichment.
