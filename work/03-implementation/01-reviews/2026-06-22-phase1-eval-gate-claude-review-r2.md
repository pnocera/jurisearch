I have everything I need. I've verified the fix end-to-end against the durable storage path, the gate logic, the tests, the evidence JSON, and the docs. Here is my R2 review.

---

## Review R2: Phase 1 Eval & D21 Embedding-Model Gate

### 1. Findings (by severity)

**[Resolved — was R1 High] `final_embedding_model` now reads durable index state, not runtime config.**

The fix is correct and complete:
- `phase1_gate_payload` signature is now `(index, ingest_health)` (`main.rs:3637`); the runtime `embedding_config`/`manifest()` is no longer threaded into the gate. Both — and the only two — callers (`status_payload:3602`, the unit test) use the new signature (confirmed via callers query).
- `phase1_embedding_model_locked(ingest_health)` (`main.rs:3735`) reads `ingest_health["embedding_manifest"]` and requires all four fields to match the locked contract: `embedding_fingerprint == "bge-m3:1024:normalize:true"`, `model == PHASE0_EMBEDDING_MODEL` (`"bge-m3"`), `dimension == PHASE0_EMBEDDING_DIMENSION` (`1024`), `normalize == true`.
- That manifest is genuinely durable: `load_ingest_health` (`ingest_accounting.rs:591-602`) reads `index_manifest WHERE key = 'embedding'`, which is written by `finalize_dense_rebuild` (`dense.rs:163-191`) from the `DenseRebuildSpec` at embed/rebuild time — i.e. how the index *was actually built*, persisted across processes. `validate_dense_spec` (`dense.rs:222-233`) enforces `fingerprint == "{model}:{dimension}:normalize:{normalize}"` and `dimension == 1024` before the manifest is written, so the four stored fields are internally consistent.

This is exactly the durable-state check R1 asked for ("compare the locked contract against the *stored* fingerprint … not the process config"). The R1 false-positive (default config attesting an arbitrarily-embedded index) is closed.

**Fail-closed behavior is correct.** `pending_ingest_health()` (`main.rs`) does not emit `embedding_manifest`, so when no index is configured/available, `manifest["embedding_fingerprint"].as_str()` is `None` → gate returns `false` → `final_embedding_model = fail`. The missing-manifest test (`main.rs:4790-4795`) locks this.

**[Resolved — was R1 Low] Test coverage now spans all branches.** `phase1_gate_payload_maps_ready_inputs_and_failed_members` (`main.rs:4709`) now exercises locked→pass plus one failing case per field: wrong model, wrong dimension (768), wrong normalize (false), wrong fingerprint, and empty manifest — each asserting `fail`. The 4-way `&&` is fully covered.

**[Low] Artifact hygiene — one 0-byte scratch file to remove before commit.** `work/03-implementation/01-reviews/2026-06-22-phase1-eval-gate-claude-review-r2.md.tmp` is 0 bytes, untracked — the scratch output of this review cycle (same pattern R1 flagged for its own `.tmp`). Delete before commit. The `…-claude-prompt.md`, `…-claude-r2-prompt.md`, and `…-claude-review.md` are legitimate tracked-or-keepable artifacts.

### 2. Pooling limitation — accurately stated and non-blocking

Confirmed accurate. The durable manifest (`dense.rs:163-180`) persists `embedding_fingerprint`, `model`, `dimension`, `normalize`, `provisional`, `reembeddable`, `vector_index`, `coverage` — **no pooling field** — and the fingerprint format `{model}:{dimension}:normalize:{normalize}` doesn't encode pooling either. So pooling genuinely cannot be verified from stored state today, and the R2 implementation correctly does not claim to. The gate verifies the locked persisted contract that exists, which is the right scope for this fix. Non-blocking.

### 3. Evidence & plan wording — accurate

- Status JSON (`…-phase1-status-after-d21-gate-fix.json`): `ingest_health.embedding_manifest` present with `bge-m3:1024:normalize:true` / `bge-m3` / `1024` / `normalize=true`, full coverage `1,852,745 / 1,852,745`, `final_embedding_model: pass`, and `claim_allowed: false` (held by `release_gating_eval_fixtures` + `reranker_decision` pending). Internally consistent.
- Benchmark summary `:94`: "pass from the stored `ingest_health.embedding_manifest`, not from transient runtime embedding config" — accurate; the manifest excerpt (`:77-84`) matches the JSON byte-for-byte.
- IMPLEMENTATION_PLAN `:727`: "the D21 final embedding-model gate now passes from the stored dense embedding manifest when it matches the locked v1 fingerprint … even while the manifest remains `provisional=true` / `reembeddable=true`" — accurate.

### 4. Open questions / residual risks (none blocking)

- **Manifest is self-attested by the rebuild path.** The gate trusts the recorded `index_manifest` row rather than independently recomputing from the raw `chunk_embeddings.model`/`dimension` columns (which `load_replay_snapshot` already hashes at `ingest_accounting.rs:807`). A stronger future check could cross-validate the manifest against that aggregate. Enhancement, not a regression — durable manifest is what R1 requested.
- **Pooling**, as above — if D21 conceptually locks pooling (e.g. `cls`), persisting and verifying it would require a manifest/schema addition. Tracked, disclosed.
- The two genuine release blockers (named-human release-gating review; reranker adoption/deferral) remain `pending` and correctly hold `claim_allowed=false`.

### Verification notes

- Confirmed durable write path: `finalize_dense_rebuild` → `index_manifest['embedding']` (`dense.rs:163-191`); read path: `load_ingest_health` (`ingest_accounting.rs:591-602`); `IngestHealthReport.embedding_manifest` field serialized (`:138`).
- Confirmed only 2 callers of `phase1_gate_payload`, both on the new signature; no residual `embedding_config.manifest()` feeding the gate.
- Confirmed constants `PHASE0_EMBEDDING_MODEL = "bge-m3"`, `PHASE0_EMBEDDING_DIMENSION = 1024` (`jurisearch-embed/src/lib.rs:13-14`).
- Confirmed all 4 field-mismatch branches + missing-manifest are tested.
- Parsed the refreshed status JSON; gate states and manifest excerpt match the summary/plan.
- Did **not** re-run `cargo test`/`build`/`clippy` (review-only, consistent with R1); test correctness verified by inspection. The storage test cited (`dense_rebuild_requires_full_coverage_then_writes_index_and_manifest`) requires Postgres and was not executed here.

VERDICT: GO

One pre-commit action: `rm work/03-implementation/01-reviews/2026-06-22-phase1-eval-gate-claude-review-r2.md.tmp`. It's cosmetic and doesn't affect the verdict.
