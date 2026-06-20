# Conception review — `jurisearch` conception document

Date: 2026-06-20
Scope: review of `work/02-conception/CONCEPTION.md` against the locked design (`work/01-design/DESIGN.md`, `work/01-design/DECISIONS.md`), the foundation (`work/00-foundation/search.md`, `work/00-foundation/assessment.md`), and the prior review chain. This is the review the lock-readiness review asked for once the conception document existed.

## Verdict

The conception document is **sound and ready to serve as the locked conceptual reference after three reconciliations.** It faithfully captures the locked product decisions, it correctly carries every non-negotiable invariant from the assessment, and — importantly — it respects the boundary the lock-readiness review drew: it is a *conception*, not an implementation plan. No architecture should be reopened.

It should not be treated as final-final with its current wording, because it carries **two divergences from the design it claims to derive from** and **one foundation pillar that has silently disappeared**. None is an architecture problem; all three would otherwise leave the "locked reference" disagreeing with `DESIGN.md`, which is exactly the failure mode the lock-readiness review warned against ("the conception document must not carry two incompatible domain models").

## Faithfulness check — locked decisions

Every locked decision in `DECISIONS.md` is represented in the conception:

| Locked decision | In conception | Faithful? |
|---|---|---|
| D1 name `jurisearch` | §1, §16 | yes |
| D2 Rust runtime / Python offline-only | §6 Runtime Boundary, §16 | yes |
| D3 embedded Postgres + `pgvector` + `pg_search` (+ fallback precedence) | §8, §16 | yes — precedence matches `DESIGN §13.3` exactly |
| D5 OpenAI-compatible endpoint default, incl. local loopback | §9, §16 | yes |
| D6 CLI-only + JSONL session, no MCP/HTTP/`serve` | §2, §16 | yes |
| D7 official DILA/LEGI XML from day 1; derived = comparison-only | §4, §16 | yes |
| D8 phased best-in-class claim (LEGI@P1, juridic@P2) | §1, §15 | yes |
| D11 reranker benchmark-gated, pluggable disabled/local/http | §9 | yes |
| D12 inline help is the contract | §10, §16 | yes |
| D15 rapprochements/applied texts are relationships, not zones | §5, §6 | yes — the P1 contradiction the lock-readiness review flagged is gone |
| D16 citation-verification states | §10 | yes — all six states present |
| temporal as-of semantics + sentinel normalization (D20) | §11, §5 | yes |

The lock-readiness cleanup landed: the zones-vs-relationships model is now consistent, the backend is described as *selected* (validation, not choice), and derived datasets are framed as comparison-only throughout.

## What the conception gets right

- **It stays conceptual.** No crate layout, no spike mechanics, no packaging criteria, no TOML, no build steps — exactly the boundary the lock-readiness review (§"Keep the conception document conceptual") asked for. The CLI command names and citation states appear because they *are* the product contract, which is legitimate at the conceptual level.
- **The invariants are the right ones and are stated as invariants** (§6): official provenance, temporal correctness, structure preservation, citation verifiability, relationship discipline, token discipline, runtime boundary. These map cleanly onto the assessment's "non-negotiable implementation constraints."
- **Temporal correctness is treated as the differentiator** (§11), not an enhancement — matching the assessment's central thesis, including the open-ended-validity `valid_to = null` normalization with raw sentinels preserved as provenance.
- **The graph discipline survives** (§12): ranked candidate edges with authority signals, explicitly *not* an autonomous "jurisprudence constante" verdict.
- **"Best-in-class" is framed as an evidence claim** with named quality gates (§13), not a slogan.

## Findings

### P1 — Pillar 2 (legal vocabulary mapping / query expansion) has disappeared

The foundation lists seven pillars; `DESIGN §1` maps **Pillar 2 = "Hybrid retrieval + legal vocabulary mapping"** to `§7 Retrieval` **and** `§9 Vocabulary`. The conception keeps the hybrid-retrieval half but **drops the vocabulary-mapping half entirely**:

- `CONCEPTION.md:208` (the §7 retrieval flow) starts at "parse the agent query and filters" and goes straight to pre-filters — there is **no expansion step**, whereas `DESIGN §7.2` opens the pipeline with "(optional) legal vocabulary expansion (§9)".
- The `expand` command (`DESIGN §10.1`, a first-class agent-facing command supporting the Pillar 7 agentic loop) is **absent from §10's command inventory**.
- There is no conceptual section corresponding to `DESIGN §9` ("lay language → formal legal terminology", e.g. "virer un employé" → "licenciement").

This is the most substantive gap: a locked conceptual reference silently omits one of the seven foundation pillars. Required fix — pick one and make it explicit:

- **Restore it:** add a one-paragraph "Vocabulary / query expansion" concept (offline-built lexicon applied to the lexical leg; exposed via `--expand` and `expand`; logged in results for grounding), and add expansion as step 0/1 of the §7 flow; add `expand` to §10. **or**
- **De-scope it on purpose:** state in §15 (or §1's phase list) that legal-vocabulary expansion is deferred (e.g. to Phase 2/3) and why. Silent omission is the only unacceptable option, because the foundation explicitly enumerates seven pillars.

### P1 — `context` is defined differently than in the design

`CONCEPTION.md:302` says: "`context`: assembles a bounded research bundle." `DESIGN.md:314` defines `context <id>` as a **structural neighborhood** command — "ancestry path + sibling articles (codes) or other zones (decisions)", flags `--up`/`--siblings` (also `DESIGN §12`: `context --siblings --as-of` reconstructs a section at a date). "Bounded research bundle" is a different operation (aggregation/assembly), not structural navigation.

The two canonical documents now describe the same agent-facing command incompatibly. Required fix: reconcile so the locked reference matches `DESIGN`. Either restate §10 as "structural neighborhood: ancestry and siblings/zones," or, if a bundle-assembly command is genuinely intended, update `DESIGN §10.1` to match and give the two distinct names. This is the same class of defect (two incompatible models of one product surface) that the lock-readiness review treated as P1 for zones.

### P2 — Decision-chunking conception omits the named French-legal failure modes

The assessment treats two decision-chunking behaviours as non-negotiable (`assessment.md` "Decision chunking must be zone-aware"; `DESIGN §6`, invariant `DESIGN:65`): (a) **zone fragments can be non-sequential**, so chunks are reassembled by *zone identity*, not text position; and (b) when official offsets are absent, **heuristic/regex splitting is a flagged fallback** (`chunking: heuristic`) so the agent and eval set know boundaries are approximate.

`CONCEPTION §5` lists the zones correctly but states neither behaviour. The "Structure Preservation" invariant (`CONCEPTION:180`) only rejects fixed-size splitting; it does not capture non-sequential reassembly or the flagged-fallback concept. These are conceptual correctness rules specific to French legal data, not implementation detail — they belong in the canonical object model. Required fix: add one or two sentences to §5's Chunk definition (and/or §6 invariants) covering zone-identity reassembly and the flagged heuristic fallback with recorded boundary provenance.

### P2 — JSONL session is a "product surface" but its conceptual contract is unstated

§2 and §10 promote `session --jsonl` to a first-class product surface, but the conception never states the conceptual shape of the contract that makes it usable as one: stable request/response correlation, stdout-is-structured-output-only / diagnostics-to-stderr, and stable error/exit semantics (`DESIGN §10.2`, `§11.1`, D17). §3 promises "deterministic error semantics" and §10 promises "Diagnostics belong on stderr," so the pieces are nearly there. The wire envelope and exit-code *vocabulary* are correctly left to `DESIGN` (implementation detail), but a conceptual reference that elevates the session to a product surface should state, in one sentence, that the session/one-shot contract is order-preserving, correlatable, and has stable machine-readable success/error semantics. Minor; close the gap or accept the deferral explicitly.

### P3 — Command inventory completeness note

§10 omits `expand` (covered under P1), and the admin/secondary commands `batch`, `model fetch`/`setup`, and `ingest`/`sync`. Dropping admin commands from a conceptual doc is reasonable and need not change. But the **model-cache honesty rule** (D19) — the thing that keeps "offline at query time" truthful for the in-process backend — has no conceptual trace; §9 mentions the in-process backend without it. Optional: one clause in §9 noting that the optional in-process backend must pre-fetch models and fail rather than download silently. Low priority because in-process is explicitly the non-default path.

## Conceptual-vs-implementation boundary

Pass. The conception avoids crate/module breakdowns, milestones, build scripts, spike task lists, and config files (the list the lock-readiness review said to avoid). The few near-implementation specifics that remain — the embeddings fingerprint field list (§9), the citation-state vocabulary (§10), the fallback precedence (§8) — are concept definitions or product-contract elements, not build instructions, and are appropriate here.

## External assumptions

Not re-verified in this pass; the lock-readiness review checked the external technical/source facts (Légifrance open data, `pgvector`, `pg_search`/ParadeDB on PG14+, `pg-embed` managed-process model, `llama.cpp` `/v1/embeddings` pooling requirement, `AgentPublic/legi` as a derived dataset) on 2026-06-20 and the conception does not introduce any new external claim beyond what was checked. The §17 reference trail is consistent with those checks.

## Answer to the lock question

Yes — the conception is **architecturally sound and faithful to the locked design.** Lock it as the canonical reference after:

1. resolving the Pillar 2 / `expand` omission (restore or explicitly de-scope) — **P1**;
2. reconciling the `context` definition with `DESIGN §10.1` — **P1**;
3. folding the non-sequential-reassembly + flagged-heuristic-fallback chunking concepts into §5/§6 — **P2**.

The P2 session-contract clause and P3 model-cache clause are polish, not blockers.

After these, `CONCEPTION.md` and `DESIGN.md` will agree on every agent-facing concept, and the conception can stand as the locked reference with `DESIGN.md` as the richer design/research record — exactly the split the lock-readiness review recommended.

## Reference trail

- Reviewed: `work/02-conception/CONCEPTION.md`
- Against: `work/01-design/DESIGN.md`, `work/01-design/DECISIONS.md`, `work/01-design/RESEARCH.md`
- Foundation: `work/00-foundation/search.md`, `work/00-foundation/assessment.md`
- Prior reviews: `work/reviews/2026-06-20-design-review.md`, `…-updated-design-review.md`, `…-direction-lock-review.md`, `…-open-questions-review.md`, `…-lock-readiness-review.md`
