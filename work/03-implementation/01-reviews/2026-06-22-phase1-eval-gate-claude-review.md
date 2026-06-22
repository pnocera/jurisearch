I have everything I need. Here's my review.

---

## Review: Phase 1 Eval & D21 Embedding-Model Gate

### 1. Findings (by severity)

**[High] The `final_embedding_model` gate validates the active *runtime config*, not the index's *stored* embedding fingerprint — so it passes by default regardless of how the index was actually embedded.**

`crates/jurisearch-cli/src/main.rs:3645` feeds `phase1_embedding_model_locked` the manifest from `status_payload`, which is built at `main.rs:3599` as `embedding_config.manifest()`. That `embedding_config` is the loaded runtime config, defaulting to `EmbeddingConfig::phase0_bge_m3(...)` (`main.rs:2822`), whose fingerprint is *always* `bge-m3 / 1024 / normalize / cls` unless overridden by `JURISEARCH_EMBED_*` env vars. So `phase1_embedding_model_locked` returns `true` in the default configuration unconditionally.

Crucially, the gate never reads durable index state:
- `load_embedding_coverage` (`crates/jurisearch-storage/src/ingest_accounting.rs:723`) only checks chunk↔embedding *internal consistency* (`ce.embedding_fingerprint = c.embedding_fingerprint`), not equality to the locked contract.
- The status path never calls `ensure_matches_index` and never reads the stored `chunks.embedding_fingerprint` (confirmed: those reads exist only in the query/embed paths at `main.rs:763/2090`, not in `status_payload`/`phase1_gate_payload`).

Concrete false-positive: an index embedded entirely with, say, a 768-dim model would have `embedding_coverage = 100%` (internally consistent) **and** `final_embedding_model = pass` (default runtime config is bge-m3). Both gate checks go green while the index violates D21. The durable signal needed to close this is already present and cheap — `chunks.embedding_fingerprint` / `chunk_embeddings.embedding_fingerprint` (storage form `bge-m3:1024:normalize:true`, see `dense.rs:249`) and the `manifests` table. The check should compare the locked contract against the *stored* fingerprint (e.g. require every stored fingerprint to equal the locked `storage_embedding_fingerprint()`), not the process config.

Why this matters here specifically: before this change the check was effectively inert (default config is `provisional=true`, so it stayed `pending` forever and never gated anything). This change promotes it to a **load-bearing, fail-closed green check** that a named human reviewer will rely on as proof the index meets D21 — but it attests the CLI's default config, not the index. The plan's "Done" bullet ("the D21 final embedding-model gate now passes for the locked v1 fingerprint") reads as an index attestation; the implementation is a config attestation.

**[Low] New tests cover only the model-name branch of the 4-way `&&`.** `main.rs:4764` flips `model` to `"other-model"`; the `dimension`, `normalize`, and `pooling` branches of `phase1_embedding_model_locked` are untested. The provisional-locked→pass case (`main.rs:4756`) is good and directly encodes the intent. Adding one mismatch case per remaining field would lock the contract.

**[Low] Stray empty temp file staged area pollution.** `work/03-implementation/01-reviews/2026-06-22-phase1-eval-gate-claude-review.md.tmp` is 0 bytes and untracked — it's the scratch output of this very review. Remove before commit. (The `…-claude-prompt.md` and `…-openrouter-dense-projection-run.log` are fine — the log is already tracked.)

### 2. Evidence accuracy (verified)

I parsed every eval JSON and cross-checked against the summary/plan — **all numbers match exactly**:

| Mode | JSON `summary` | Doc claim | ✓ |
|---|---|---|---|
| BM25 | passed 4 / failed 0, 16.62s | 4/4, 16.62s | ✓ |
| dense | passed 2 / failed 2, 14.09s | 2/4, 14.09s | ✓ |
| hybrid | passed 4 / failed 0, 21.24s | 4/4, 21.24s | ✓ |
| hybrid +dev | passed 5 / failed 1, 35.3s | 5/6, 35.30s | ✓ |

The dense failures (`veterinaire-deontologie-2003`, `reserve-naturelle-r242-41-1990`) and the dev failure (`legi-hierarchy-temporal-sibling-2000`) match the artifact. The plan's honest caveat — "the current four fixtures do not [prove hybrid beats BM25], because BM25 and hybrid both pass" — is well-stated and correct; it's the most important qualitative truth in this evidence and it's surfaced, not buried.

The refreshed status artifact matches the live `phase1_gate.checks` (final_embedding_model: pass; release_gating_eval_fixtures + reranker_decision: pending; claim_allowed: false). `provisional`/`reembeddable` remain `true`, preserving migration capability as intended.

### 3. Remaining blockers (correct)

The two stated blockers — named-human release-gating review and the reranker adoption/deferral decision — are correctly modeled as `pending`, and `claim_allowed=false` is correctly held. **Note:** because of Finding [High], once those two are resolved the claim would open with `final_embedding_model` green without any durable proof the index used the locked model. So I'd treat "harden `final_embedding_model` to read stored state" as a *third* blocker before the claim can open, not optional polish.

### 4. Open questions / residual risks
- Was config-attestation (vs. stored-state attestation) a deliberate design choice for `final_embedding_model`? If yes, the gate message and the plan bullet should say "active config" rather than implying the index is verified, and the genuine index check should live elsewhere.
- The replay-snapshot O(full-corpus) cost on every `status` call (12.9M edges, flagged in the plan follow-up) means this gate is expensive to poll — worth the materialization the plan proposes, but out of scope here.

### Verification notes
- Verified `embedding_config.manifest()` is runtime-derived and defaults to the locked fingerprint (`main.rs:2822`, `lib.rs:114`).
- Verified `load_embedding_coverage` checks only internal consistency, not the locked contract (`ingest_accounting.rs:723`).
- Verified `status_payload`/`phase1_gate_payload` never read stored fingerprints or call `ensure_matches_index`.
- Parsed all 4 eval JSONs + time files; counts/timings/fixture IDs match docs.
- Did not re-run `cargo`/`clippy`; relied on the stated validation runs (review-only, no build executed).

VERDICT: FIXES_REQUIRED
