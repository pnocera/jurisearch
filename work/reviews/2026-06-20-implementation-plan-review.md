# Implementation plan review — `jurisearch`

Date: 2026-06-20
Scope: review of `work/03-implementation/IMPLEMENTATION_PLAN.md` against the locked conception (`work/02-conception/CONCEPTION.md`), the design record (`work/01-design/DESIGN.md`, `DECISIONS.md`, `RESEARCH.md`), and the foundation. Question: is the plan sound and ready to execute?

## Verdict

**Yes — the plan is sound and ready to drive execution, after folding in a small number of additions.** It is faithful to every locked decision, it does not reopen architecture, and its single most important property is correct: **validation gates are sequenced ahead of feature work** (Phase 0 + §10 first-batch order). The workstream/phase/gate/risk structure is the right shape for an execution plan, and nearly every subsection carries testable acceptance criteria.

It is not yet a *complete* execution plan. Three gaps would cause real rework or stall mid-build if not addressed first (P1 below): the embedding-model deferral has an unplanned re-embedding/migration cost; the official-API client (OAuth2/rate-limit) is needed by Phase 1 `cite --online` and Phase 2 `sync` but has no task; and the two decompositions (workstreams vs phases) are never mapped to each other, leaving ownership and intra-phase dependencies implicit. None is an architecture problem.

## What the plan gets right

- **Validation-first sequencing.** Phase 0 (§3) and the "Immediate Next Work" batch (§10) put the backend spike, ingestion spike, embeddings-fingerprint guard, and eval harness *before* features. This is exactly the posture the conception-readiness review asked for and the biggest determinant of whether this avoids a toy path.
- **Eval harness as Phase 0 infrastructure** (W2, 0.2), not a late add — correctly treated as the gate for "best-in-class," with a held-out split and per-category reporting.
- **Best-in-class claims are gated and phase-scoped** (1.7, 2.6, §8): Phase 1 may claim only LEGI/statutory; full juridic is gated to Phase 2. `status` is forbidden from over-claiming. Matches D8.
- **Faithful to the lock.** §1 restates the locked decisions and the fallback precedence verbatim, with an explicit no-reopen rule. Fallbacks are framed as hard-failure-only.
- **Both optional items from the readiness review were absorbed:** authority is implemented as a ranking prior, not a gate (W5; 1.3 "never overrides explicit filters"; 2.4), and `cite` is local-by-default with optional `--online` (1.4). Good evidence the review chain is being honoured.
- **Risk register exists** (§9) and covers the main technical failure modes with the locked mitigations (no jumping to Qdrant; HTTP rerank fallback; Rust-no-Python test).

## Findings

### P1 — The embedding-model deferral has an unplanned re-embedding / migration cost

Phase 0 builds dense retrieval and stores document vectors (0.4, 0.6 line 240) using *some* model, but the **final embedding model is not chosen until Phase 1 (1.7, lines 367–369)**. Per `DESIGN §11.2`, document and query embeddings must share one fingerprint and **re-embedding requires an explicit index migration declared in the manifest**. So when 1.7 picks a winner other than the provisional model, every stored vector must be recomputed and the index migrated — a cost the plan never names as a task or risk.

Required additions:
- State in Phase 0 that `bge-m3` is the **provisional** benchmark-default used until 1.7, and that all dense work before 1.7 is explicitly re-embeddable.
- Add a task (W3/W5) for the **re-embedding / index-migration path**: manifest fingerprint change, full-corpus re-embed, version bump — this also satisfies the `DESIGN §13.3` "Upgrades: index/schema/extension migration story" acceptance criterion, which currently has no dedicated task (W3 line 65/71 only says "migrations").
- Add a risk-register line: "Embedding-model winner differs from provisional → full re-embed + index migration; budget it into 1.7."

### P1 — No task for the official-API client (PISTE / Judilibre): OAuth2, rate limits, sandbox

`cite --online` (1.4, line 319), Judilibre ingestion (2.1), and `sync --since` (2.5, line 450) all depend on the official APIs, but **no subsection or workstream owns the API client itself**. Per `RESEARCH §1`, this is non-trivial: OAuth2 client-credentials auth, token refresh, PISTE rate limits, sandbox-vs-prod endpoints, and Judilibre `/transactionalhistory` for deltas. Secrets handling exists (W7) but the client does not.

Required additions:
- Add a task (W4 or a new W) for the PISTE/Judilibre client: OAuth2 client-credentials, token lifecycle, rate-limit/backoff, sandbox/prod config, error mapping to the `5` upstream exit code.
- Make it a Phase 1 dependency of 1.4 (`--online`) and a Phase 2 dependency of 2.1/2.5.
- Add a risk line for API coverage-date drift and rate limits (`RESEARCH §6`: bulk for full builds, API for deltas/verification only).

### P1 — Workstreams and phases are never mapped; intra-phase dependencies are implicit

§2 defines W1–W7; §3–6 use independent numbering (0.1–2.6) with **no cross-reference to which workstream owns each task**, and there is **no dependency/parallelization statement** within phases. For example 0.6 (Baseline Hybrid Retrieval) clearly depends on 0.3 + 0.4 + 0.5, but that is left for the reader to infer; W4 ("structure-aware statutory chunking") and Phase 1 1.2 ("Statutory Chunking") overlap without a stated owner.

Required additions:
- Add a small **traceability matrix** (phase task → owning workstream) or tag each subsection with its `Wn`.
- State per-phase dependencies / what can run in parallel (a one-line dependency note per subsection, or a short DAG). §10 gives a good global order but not the within-phase graph.

This is the difference between a readable plan and an executable one.

### P2 — Phase 0 spike targets are not made concrete

0.3 acceptance (line 195) says "under target latency" and points to `DESIGN §13.3` but does not restate the concrete bar. Implementors should not have to chase the number. Pull forward from `DESIGN §13.3`: **stable JSON < 500 ms warm**, indicative spike dataset **~50k LEGI article versions + 10k Judilibre decisions**, and the explicit packaging criteria (bundled-vs-downloaded binaries + offline-install story, pinned `pgvector`/`pg_search` builds, socket/loopback binding, single-writer lock, crash recovery/clean shutdown). Restate them as the spike checklist.

### P2 — Reranker has no Rust-inference spike scheduled (only an eval)

D11 / `DESIGN §7.2` require a **Rust inference spike** (model availability, tokenizer behaviour, ONNX/Candle compatibility, latency, packaging) *before* the adoption decision. The plan has the pluggable provider (W5), an eval in 1.7, and an HTTP-fallback risk line (572) — but no spike task analogous to 0.3. Add a reranker spike (late Phase 0 or early Phase 1) that feeds the 1.7 adoption gate, so "ship it in Phase 1 if it clears the bar" is actually reachable on schedule.

### P2 — Cross-platform target policy is unstated

`DESIGN §13.3` lists "Platforms: cross-platform target policy (documented even if v1 ships Linux-only)" as a backend acceptance criterion. The plan never names target OSes, and embedded-Postgres + extension packaging is the most platform-sensitive part of the build. Add an explicit platform decision to Phase 0 (e.g. "v1 Linux-only; macOS/Windows policy recorded").

### P2 — The eval gold-set is the largest, most expertise-bound effort and is under-scoped

W2/0.2/1.7/2.6 treat "production-grade French legal eval set" as ordinary tasks, but producing gold article IDs / ECLIs for realistic workflows needs French legal expertise and review — and it is what every best-in-class gate depends on. The risk line (571) acknowledges "too small or generic" but not the *how*. Add: who produces/validates gold labels, how labels are reviewed, and the minimum task coverage per category (known-article lookup, conceptual, as-of, jurisprudence-by-facts, citation states). Same note applies, smaller scale, to the curated vocabulary seed lexicon (1.3) — it needs a sourcing/expertise owner.

### P2 — Pseudonymisation preservation has a deliverable but no test

W7 (line 131) lists "Pseudonymisation preservation" and 2.1 preserves decision text, but no acceptance criterion or test asserts that the pipeline never reverses or cross-links to re-identify (conception §14 / `DESIGN §16` compliance invariant). Add an explicit Phase 2 acceptance/test: no re-identification, no cross-source linking that defeats source pseudonymisation.

### P3 — Minor gaps and polish

- **AGPL-3.0 distribution consequence.** The project *accepts* AGPL (locked), but bundling AGPL `pg_search` into a distributed binary triggers source-availability obligations for the combined work. Worth a one-line operational note in W7 / risk register so it is a conscious release step, not a surprise.
- **Observability.** No mention of structured runtime logging/tracing (to stderr) for debugging the engine; useful for an agent-driven tool. Optional.
- **Manifest/canonical-record retention.** 1.1 asserts reproducibility; consider stating whether canonical records are a retained build artifact or always regenerated.

## Coverage check — conception → plan

Spot-checked the conception's commitments against plan tasks; coverage is good:

| Conception element | Plan location | Covered |
|---|---|---|
| CLI command surface incl. `expand` | W6, 1.3, 1.4, 1.5 | yes |
| Inline help / `help schema` no-index | W1, 0.1 | yes |
| Temporal as-of + sentinel normalization | 0.2, 0.5, 1.1, 1.2 | yes |
| Decision zone-identity reassembly + flagged fallback | 2.1, 2.2 | yes |
| Relationship discipline (rapprochements/applied ≠ zones) | 2.1, 2.3 | yes |
| Citation states + `--strict` + local/`--online` | 1.4, 2.4 | yes |
| Embeddings fingerprint hard check | 0.4, 1.6 | yes |
| Authority = signal not gate | W5, 1.3, 2.4 | yes |
| Vocabulary expansion (lexical leg only) | W5, 1.3 | yes |
| Model-cache honesty rule | W7, 0.4, 1.6 | yes |
| JSONL session contract | 1.5 | yes |
| Eval-gated, phase-scoped best-in-class | 1.7, 2.6, §8 | yes |
| Embedding-model migration when winner changes | — | **gap (P1)** |
| Official-API client (OAuth2/rate limit) | — | **gap (P1)** |

## Bottom line

Green-light execution. The plan's sequencing and gating are right, and it faithfully implements the locked conception. Before the first build batch, fold in the three P1 additions (re-embedding/migration plan; API-client workstream; workstream↔phase mapping + dependencies) and ideally the P2 set (concrete spike targets, reranker spike, platform policy, eval gold-set sourcing, pseudonymisation test). With those, this is a solid, low-ambiguity execution plan.

## Reference trail

- Reviewed: `work/03-implementation/IMPLEMENTATION_PLAN.md`
- Against: `work/02-conception/CONCEPTION.md`, `work/01-design/DESIGN.md`, `work/01-design/DECISIONS.md`, `work/01-design/RESEARCH.md`
- Foundation: `work/00-foundation/search.md`, `work/00-foundation/assessment.md`
- Prior reviews: `work/reviews/2026-06-20-conception-readiness-review.md` and the conception/design-stage reviews
