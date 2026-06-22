# Claude Review Prompt R2: Phase 1 Eval and D21 Gate Fixes

Repo: `/home/pierre/Work/jurisearch`

Review scope:
- R2 review after `VERDICT: FIXES_REQUIRED` in `2026-06-22-phase1-eval-gate-claude-review.md`.
- Do not edit files. Review only.

R1 blocking finding:
- `final_embedding_model` checked transient runtime embedding config instead of durable index state.

Fix applied:
- `crates/jurisearch-storage/src/ingest_accounting.rs`
  - `IngestHealthReport` now includes `embedding_manifest`.
  - `load_ingest_health` reads durable `index_manifest` row `key = 'embedding'`.
- `crates/jurisearch-cli/src/main.rs`
  - `phase1_gate_payload` now receives only `index` and `ingest_health`.
  - `final_embedding_model` now checks `ingest_health.embedding_manifest`, not `embedding_config.manifest()`.
  - The gate passes only when stored manifest fields match:
    - `embedding_fingerprint == "bge-m3:1024:normalize:true"`
    - `model == "bge-m3"`
    - `dimension == 1024`
    - `normalize == true`
  - It fails on wrong model, wrong dimension, wrong normalization, wrong fingerprint, or missing manifest.
- `work/03-implementation/02-evidence/2026-06-22-phase1-status-after-d21-gate-fix.json`
  - Refreshed after rebuild. It now includes `ingest_health.embedding_manifest` with full dense coverage and `final_embedding_model: pass`.
- `work/03-implementation/02-evidence/2026-06-22-phase1-eval-benchmark-summary.md`
  - Updated to state the gate is based on stored `ingest_health.embedding_manifest`.
- `work/03-implementation/IMPLEMENTATION_PLAN.md`
  - Updated to say the D21 gate passes from the stored dense embedding manifest.

Known limitation:
- The existing dense manifest and storage fingerprint persist `model`, `dimension`, `normalize`, and storage fingerprint, but not pooling. The R2 implementation therefore does not claim to durably verify pooling. It verifies the locked persisted contract that exists today.

Validation rerun after R2 fix:

```bash
cargo fmt --all
cargo test -p jurisearch-cli phase1_gate_payload_maps_ready_inputs_and_failed_members
cargo test -p jurisearch-storage dense_rebuild_requires_full_coverage_then_writes_index_and_manifest
cargo build -p jurisearch-cli
JURISEARCH_CONFIG=none target/debug/jurisearch \
  --index-dir /home/pierre/Work/jurisearch/index/phase1-freemium-20250713 \
  status > work/03-implementation/02-evidence/2026-06-22-phase1-status-after-d21-gate-fix.json
cargo test -p jurisearch-cli
cargo test -p jurisearch-storage dense_rebuild_requires_full_coverage_then_writes_index_and_manifest
cargo clippy --workspace --all-targets -- -D warnings
```

Please review:
1. Whether R1 high finding is fixed.
2. Whether the remaining pooling limitation is accurately stated and non-blocking for this persisted-contract fix.
3. Whether the evidence and plan wording are now accurate.
4. Whether any remaining artifact hygiene issue exists before commit.

Output format:
1. Findings first, ordered by severity, with file/path references where applicable.
2. Open questions or residual risks.
3. Verification notes.
4. Final line exactly one of:
   - `VERDICT: GO`
   - `VERDICT: FIXES_REQUIRED`
