# Re-Review: BSARD external benchmark harness + Phase 1 gate (R2)

I reviewed the full uncommitted diff against the five prior FIXES_REQUIRED findings plus the LOW operational bucket. Each is addressed below.

## Findings (by severity)

### Prior #1 (HIGH) — Trivially-low thresholds / trusts self-reported `state` → **RESOLVED**
The gate no longer trusts `state` alone. `phase1_validate_external_benchmark_metric` (`main.rs:4028`) enforces two independent layers per metric:
- **Policy floor**: `thresholds.hybrid_{recall,ndcg,mrr}_at_20_min` must clear `0.75 / 0.60 / 0.50` (`main.rs:89-91`).
- **Re-derivation**: `metrics.hybrid.* >= threshold` is checked directly, so pass is re-derived from metrics, not copied from the producer.

Net effect: a passing artifact requires `metric ≥ threshold ≥ floor`. The zero-threshold attack from the prior review is now blocked (covered by `external_benchmark_payload_rejects_zero_threshold_pass_artifact`), and the smoke evidence (`...-smoke.md:50`) shows status rejecting it with the exact floor errors. `payload["state"]` is only set to `passed` when validation produces **zero** errors *and* the artifact already says `passed` — both conditions required.

### Prior #2 (MED-HIGH) — Embedding model unconstrained → **RESOLVED (with a residual note)**
The gate now asserts `embedding.fingerprint_model == bge-m3` (`PHASE0_EMBEDDING_MODEL`, `main.rs:3952`), `embedding.dimension == 1024` (`:3958`), and `embedding.normalize == true` (`:3966`). Residual (non-blocking): the Python harness hardcodes `"fingerprint_model": "bge-m3"` (`bsard_benchmark.py:472`) regardless of `--model`, so the Rust cross-check can catch a hand-edited/missing field but cannot detect a `request_model`-vs-deployed-model divergence. `request_model` is recorded for audit, and the harness is project-owned, so this is acceptable as defense-in-depth — but the fingerprint is not *derived* from the actual request.

### Prior #3 (MED) — `revision: "unknown"` accepted → **RESOLVED**
Fixed on both sides: `resolve_dataset_revision` now raises `RuntimeError` instead of returning `"unknown"` (`bsard_benchmark.py:217-226`), and the Rust gate explicitly rejects `dataset.revision == "unknown"` (`main.rs:3974`) in addition to the non-empty check. Covered by `external_benchmark_payload_rejects_unknown_dataset_revision`.

### Prior #4 (MED) — `recall_at_k` was actually success@k → **RESOLVED**
`recalls.append(retrieved_relevant / max(1, len(relevant)))` is now true recall (`bsard_benchmark.py:381-382`), with hit-rate reported separately as `success_at_{k}` (`:400`). nDCG ideal denominator uses `min(len(relevant), k)` (`:385`), consistent with binary multi-relevant nDCG.

### Prior #5 (LOW-MED) — Weak identity/size pinning → **RESOLVED**
Gate now pins `kind == phase1_external_expert_benchmark` (`main.rs:3936`), `schema_version == 1` (`:3939`), `corpus_documents >= 22000` (`:3984`), `questions >= 200` (`:3993`), and rejects non-null `limit_corpus`/`limit_questions` (`:3978`).

### Prior #6 (LOW) — Operational → **MOSTLY RESOLVED**
- Retry/backoff on 429/5xx with capped exponential sleep: added (`bsard_benchmark.py:244-260`). ✅
- Cache key now includes `base_url` and `max_input_chars` (`:277-279`). ✅
- Input truncation via `bounded_input` / `--max-input-chars` (default 24000), recorded in artifact (`:423-426`, `:476`). ✅
- Dense ties broken deterministically by `doc_id` (`:348`). ✅
- Residual (acceptable for LOW): the `.npz` is still written only after *both* doc and query embedding succeed (`:322-328`), so a full ~22k-doc run that exhausts all 4 retries still discards work. Evidence-file existence is still structurally checked only. Neither blocks the gate's fail-closed behavior.

## Open questions / risks
- **k is hardcoded to 20 in the Rust floors** (`recall_at_20` etc., `main.rs:4007-4023`) while Python takes `--k` (default 20). An artifact produced with `--k != 20` writes `*_at_<k>` keys, so the gate sees the `_at_20` keys as missing → fails closed. This is the *correct* safe outcome, but it's implicit; worth a one-line note in the README that gate artifacts must use `--k 20`.
- **fingerprint_model residual** (see #2): if you want the cross-check to be more than cosmetic, derive `fingerprint_model` from the actual model/endpoint rather than a constant. Not a blocker.

## Verification notes
- `python3 -m py_compile external-benchmarks/bsard_benchmark.py` → OK.
- `cargo test -p jurisearch-cli external_benchmark` → 5/5 pass (valid-pass, invalid-pass, zero-threshold reject, unknown-revision reject, evidence-required).
- `cargo test -p jurisearch-cli --test cli_contract` → 45 pass / 2 ignored, including the new `status_consumes_external_benchmark_artifact_from_env` (env→artifact→`external_expert_annotated_eval=pass`, `claim_allowed=false`).
- `cargo fmt --all -- --check` → clean.
- Schema additions (`source`, `artifact_path`, `artifact_error`, `dataset`, `metrics`, `thresholds`, `artifact`) accept the new payload shape; status emits with empty stderr, so runtime schema validation is satisfied.
- Diff is scoped to the benchmark gate (gate logic, schema, tests, three docs, new `external-benchmarks/`). No unrelated changes.

**Commit-safety nit (not a code finding):** there is a stray 0-byte `work/03-implementation/01-reviews/2026-06-22-bsard-external-benchmark-harness-claude-review-r2.md.tmp` in the untracked set. It won't be picked up by a targeted `git add`, but exclude/delete it before any `git add -A`.

All prior FIXES_REQUIRED findings are resolved with tests covering each; residuals are LOW/informational and fail-closed.

VERDICT: GO
