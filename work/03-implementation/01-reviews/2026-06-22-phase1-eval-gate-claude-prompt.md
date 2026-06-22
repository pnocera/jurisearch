# Claude Review Prompt: Phase 1 Eval and D21 Gate Update

Repo: `/home/pierre/Work/jurisearch`

Review scope:
- Phase 1 real-index eval evidence after completed OpenRouter dense projection.
- D21 `bge-m3` final embedding-model gate fix in `status`.
- Implementation plan update for Phase 1.7.
- Do not edit files. Review only.

Changed code:
- `crates/jurisearch-cli/src/main.rs`

Changed docs/evidence:
- `work/03-implementation/IMPLEMENTATION_PLAN.md`
- `work/03-implementation/02-evidence/2026-06-22-phase1-eval-benchmark-summary.md`
- `work/03-implementation/02-evidence/2026-06-22-phase1-eval-fixtures-list.json`
- `work/03-implementation/02-evidence/2026-06-22-phase1-eval-bm25-top20.json`
- `work/03-implementation/02-evidence/2026-06-22-phase1-eval-bm25-top20.time.json`
- `work/03-implementation/02-evidence/2026-06-22-phase1-eval-dense-top20.json`
- `work/03-implementation/02-evidence/2026-06-22-phase1-eval-dense-top20.time.json`
- `work/03-implementation/02-evidence/2026-06-22-phase1-eval-hybrid-top20.json`
- `work/03-implementation/02-evidence/2026-06-22-phase1-eval-hybrid-top20.time.json`
- `work/03-implementation/02-evidence/2026-06-22-phase1-eval-hybrid-include-dev-top20.json`
- `work/03-implementation/02-evidence/2026-06-22-phase1-eval-hybrid-include-dev-top20.time.json`
- `work/03-implementation/02-evidence/2026-06-22-phase1-status-after-d21-gate-fix.json`

Intent:
- The plan says `bge-m3` is locked by D21, and no Phase 1 model-selection bake-off remains.
- Before this change, `status.phase1_gate.final_embedding_model` stayed pending solely because the manifest is `provisional=true`, even when the active fingerprint is the locked D21 model.
- The fix makes the gate pass only when the active fingerprint matches the locked v1 model contract: `bge-m3`, 1024 dimensions, normalized, CLS pooling. A non-locked model now fails that check.
- The manifest can remain `provisional=true` / `reembeddable=true` to preserve migration capability.

Evidence summary:
- Completed index: `/home/pierre/Work/jurisearch/index/phase1-freemium-20250713`
- Backup: `/mnt/models/jurisearch-backup/phase1-freemium-20250713-20260622T135908+0200`
- Release-gating evals at top 20:
  - BM25: 4/4 pass, 16.62s
  - dense: 2/4 pass, 14.09s
  - hybrid: 4/4 pass, 21.24s
- Hybrid include-dev: 5/6 pass, 35.30s
  - Failing dev fixture: `legi-hierarchy-temporal-sibling-2000`
- Refreshed status after the gate fix:
  - `index_query_ready`: pass
  - `latest_completed_ingest_run`: pass
  - `failed_members`: pass
  - `projection_coverage`: pass
  - `embedding_coverage`: pass
  - `replay_snapshot`: pass
  - `final_embedding_model`: pass
  - `release_gating_eval_fixtures`: pending
  - `reranker_decision`: pending

Validation already run:

```bash
cargo fmt --all
cargo test -p jurisearch-cli phase1_gate_payload_maps_ready_inputs_and_failed_members
cargo build -p jurisearch-cli
cargo test -p jurisearch-cli
cargo clippy --workspace --all-targets -- -D warnings
```

Live commands run for evidence:

```bash
JURISEARCH_CONFIG=none \
JURISEARCH_EMBED_BASE_URL=http://127.0.0.1:8097/v1 \
target/debug/jurisearch \
  --index-dir /home/pierre/Work/jurisearch/index/phase1-freemium-20250713 \
  eval phase1 --mode <bm25|dense|hybrid> --top-k 20

JURISEARCH_CONFIG=none \
JURISEARCH_EMBED_BASE_URL=http://127.0.0.1:8097/v1 \
target/debug/jurisearch \
  --index-dir /home/pierre/Work/jurisearch/index/phase1-freemium-20250713 \
  eval phase1 --include-dev --mode hybrid --top-k 20

JURISEARCH_CONFIG=none \
target/debug/jurisearch \
  --index-dir /home/pierre/Work/jurisearch/index/phase1-freemium-20250713 \
  status
```

Please review whether:
1. The D21 gate fix is correct and does not accidentally allow non-locked/provisional models.
2. The Phase 1 eval evidence is accurately summarized in the plan and summary artifact.
3. The remaining blockers are correct: named-human release-gating review and reranker decision.
4. Any evidence artifact should be added/removed before commit.

Output format:
1. Findings first, ordered by severity, with file/path references where applicable.
2. Open questions or residual risks.
3. Verification notes.
4. Final line exactly one of:
   - `VERDICT: GO`
   - `VERDICT: FIXES_REQUIRED`
