# Conception readiness review — `jurisearch` (pre-implementation-plan)

Date: 2026-06-20
Scope: re-review of `work/02-conception/CONCEPTION.md` after reconciliation against `work/reviews/2026-06-20-conception-review.md`. Question asked: **is the conception sound enough to proceed to an implementation plan?**

## Verdict

**Yes — proceed to the implementation plan.** Every finding from the prior conception review is resolved, the conception is internally consistent, it agrees with `DESIGN.md`/`DECISIONS.md` on every agent-facing concept, and all seven foundation pillars are now represented. There is no architecture or conceptual blocker. Two optional polish items remain (below); neither should hold up planning, and both are already covered in full by `DESIGN.md`, which the implementation plan will draw from anyway.

## Resolution of prior findings

| Prior finding | Status | Where |
|---|---|---|
| **P1** Pillar 2 (legal vocabulary mapping / query expansion) dropped | **Resolved** | §7 flow step 2 (`CONCEPTION:211`); new "Vocabulary and Query Expansion" subsection (`:226-230`); `expand` in command inventory (`:314`); lock clause (`:494`) |
| **P1** `context` defined incompatibly with `DESIGN §10.1` | **Resolved** | `:313` now reads "structural neighbourhood … ancestry path and sibling articles for codes, or neighbouring zones for decisions" — matches `DESIGN.md:314` |
| **P2** decision-chunking failure modes (non-sequential reassembly, flagged heuristic fallback) absent | **Resolved** | Chunk definition (`:154`) and Structure Preservation invariant (`:184`) |
| **P2** JSONL session promoted to product surface but its conceptual contract unstated | **Resolved** | Output Concepts (`:324`): correlatable, order-preserving, stable success/error shapes, stdout structured-only |
| **P3** model-cache honesty rule (D19) had no conceptual trace | **Resolved** | §9 (`:275`): explicit fetch before query, fail-not-silent-download |

The reconciliations introduced no new contradictions: the vocabulary text matches `DESIGN §9`, `context` matches `DESIGN §10.1`, chunking matches `DESIGN §6` and `D10`/`D15`, the session contract matches `DESIGN §11.1`/`D17`, and the model-cache rule matches `D19`. Traceability is good — the status line and reference trail (`:4`, `:514`) now cite the prior review.

## Fresh pass — is anything else needed before planning?

I checked the conception specifically for gaps that would make an implementation plan guess at intent. None are blocking. The conception correctly *defers* implementation detail (exit-code vocabulary, wire-envelope shape, crate layout, spike acceptance criteria, config schema) to `DESIGN.md`; that deferral is the intended design, not a gap. The plan should be built from `DESIGN.md` (the richer detail record) governed by `CONCEPTION.md` (the locked concepts).

Two optional, non-blocking polish items:

### Opt-1 — Authority weighting as a signal, not a gate (Pillar 5)

The conception uses signal language ("authority-aware ranking signals", §7 step 8; "authority signals", §12) but never states the explicit Pillar 5 / assessment nuance that authority is a **ranking prior, not a hard filter** — the agent can still ask for `Cour d'appel` regional trends and the prior steps aside (`DESIGN §7.3`; assessment: "They should remain ranking signals, not hard filters"). This is the symmetric twin of the graph's "must not assert *jurisprudence constante*" rule, which the conception *does* state explicitly (§12). One sentence in §7 or §12 would close the asymmetry. Not a blocker — `DESIGN §7.3` covers it fully.

### Opt-2 — `cite` local-by-default vs `--online` posture

§4 names "online citation confirmation" as a use of the PISTE/Judilibre API, and the `source_unavailable` state implies the distinction, but the conception never states the **local-by-default, offline-deterministic** posture with optional `--online` confirmation (`DESIGN §10.5`/`D16`) — a privacy/determinism stance, not pure implementation, given §3's "repeatable … loops" and §14's confidentiality framing. One clause in §10 (Citation Verification States) would make the posture explicit. Not a blocker — `DESIGN §10.5` covers it fully.

## Guidance for the implementation plan

Since the conception is sound, the plan can proceed. To keep it grounded:

- **Source split:** locked concepts come from `CONCEPTION.md`; design detail (crate layout `DESIGN §13.2`, index artifact `§13.5`, output schemas `§10.2`, wire protocol `§11.1`, config/secrets `§14`, exit codes `§10.2`) comes from `DESIGN.md`. The plan should not re-decide anything locked in §16.
- **Phase 0 is spike-first.** The three remaining uncertainties are *validation gates*, not design choices, and all live in Phase 0 before feature work: (1) backend packaging/runtime quality (embedded Postgres + `pgvector` + `pg_search`, criteria in `DESIGN §13.3`), (2) embedding-model winner under post-fusion legal evals, (3) reranker adoption under the latency/quality gate. The plan should schedule these as gated spikes with the documented fallback precedence engaged only on hard failure.
- **The eval harness is Phase 0 infrastructure, not a late add.** "Best-in-class" is eval-gated (§13), so the eval set and the CLI-contract behavioural tests must exist before Phase 1 can claim its bar.
- **Plan the contract surface as a first-class deliverable.** Inline help (`help agent`, `help schema --json`), the JSONL session protocol, and citation-verification states are product surface, not documentation — they need plan line items and eval coverage like any feature.

## Bottom line

The conception is locked-quality and consistent with the design and foundation. **Green-light the implementation plan.** Fold Opt-1 and Opt-2 in opportunistically (or rely on `DESIGN.md` for them) — they do not gate planning.

## Reference trail

- Reviewed: `work/02-conception/CONCEPTION.md`
- Against: `work/01-design/DESIGN.md`, `work/01-design/DECISIONS.md`, `work/01-design/RESEARCH.md`
- Foundation: `work/00-foundation/search.md`, `work/00-foundation/assessment.md`
- Prior reviews: `work/reviews/2026-06-20-conception-review.md` and the four design-stage reviews
