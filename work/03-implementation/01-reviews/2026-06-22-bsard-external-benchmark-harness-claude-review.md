I now have everything needed. Let me confirm one more thing: whether the gate cross-checks the benchmark's embedding model against the locked Phase 1 model.

The validation function (`main.rs:3920-3970`) never inspects `artifact.embedding.*`, and `phase1_embedding_model_locked` (`main.rs:4017`) reads only the ingest manifest — so the two are independent. That's a finding. Here's my review.

---

# Review: BSARD external benchmark harness + Phase 1 gate consumption

## Findings (by severity)

### 1. HIGH — Gate opens on trivially-low thresholds; it trusts the artifact's self-reported `state` and never enforces a quality floor

`phase1_external_benchmark_artifact_errors` (`main.rs:3951-3955`) requires only that `thresholds` and `metrics` are *present and non-null*. It never checks that the threshold values clear a policy floor, and it never re-derives pass from `metrics ≥ thresholds`. The pass decision is taken verbatim from the artifact's `state` field (`main.rs:3839` → `check_status` at `main.rs:3984-3996`), which the Python harness computes against **caller-supplied** CLI flags (`bsard_benchmark.py:425-430`, defaults at `:487-489`).

Consequence: a **full, unlimited** run with `--min-hybrid-recall-at-k 0 --min-hybrid-ndcg-at-k 0 --min-hybrid-mrr-at-k 0` produces `state:"passed"` with valid metadata and non-empty evidence → the gate accepts it → `external_expert_annotated_eval` passes. Your own smoke evidence proves the mechanism is live: the smoke artifact's internal state was `passed` at zero thresholds and was blocked **only** by the limit fields (`2026-06-22-...-smoke.md:58`). Remove the limits and the same zero-threshold run sails through.

This directly contradicts the stated intent ("fail-closed unless ... a full, valid run") — a zero-threshold full run is functionally a smoke that proves nothing, yet it is accepted. The remediation note in the evidence ("non-zero predeclared thresholds", `:59`) is documentation only; nothing in the Rust gate enforces it.

Fix: the Rust gate should (a) enforce a minimum policy floor on the relevant threshold keys (e.g. `hybrid_recall_at_20_min >= X`), and (b) as defense-in-depth across the Python/Rust trust boundary, re-derive pass from `metrics.hybrid` vs `thresholds` rather than trusting the producer's `state`.

### 2. MED-HIGH — Benchmark's embedding model is never constrained to the locked Phase 1 model

The artifact records `embedding.model` / `embedding.normalize` (`bsard_benchmark.py:450-454`), but the gate validation never inspects `artifact.embedding.*`. The `final_embedding_model` check (`main.rs:4017-4023`) validates the *index* manifest against the locked `bge-m3:1024:normalize:true`, entirely independently of the benchmark. So the external benchmark could "pass" using a different (or stronger) embedding model than the one shipped in the index, and the gate would not notice. A benchmark meant to validate the deployed retrieval stack should be required to assert `embedding.model == bge-m3` (+ normalize) matching the locked fingerprint.

### 3. MED — `revision: "unknown"` satisfies the reproducibility requirement

When no `--dataset-revision` is passed and `HfApi().dataset_info` fails (offline/transient), `resolve_dataset_revision` returns `"unknown"` (`bsard_benchmark.py:219-228`) and `load_bsard` loads the dataset at the **unpinned default branch** (`load_revision` stays `None`, `:186-189`). The artifact then records `revision:"unknown"`. The gate's `dataset.revision is required` check only tests non-empty/non-whitespace (`main.rs:3946-3950`), so `"unknown"` passes. Result: an unpinned, non-reproducible run satisfies the "dataset revision pinned" requirement. The gate should reject `"unknown"` (and the harness should arguably hard-fail rather than silently load `main`).

### 4. MED — `recall_at_k` is actually hit-rate/success@k, not recall@k

`evaluate_rankings` sets `recalls.append(1.0 if first_rank else 0.0)` (`bsard_benchmark.py:370-372`) — i.e. 1 if *any* relevant doc appears in top-k. That is success@k / hit@k, not recall@k (`|retrieved ∩ relevant| / |relevant|`). BSARD questions frequently have multiple relevant articles, so this overstates "recall" and is not comparable to published BSARD recall@k numbers. The `--min-hybrid-recall-at-k 0.75` threshold is therefore an easier bar than its name implies (compounding finding #1). Note nDCG@k (`:374`, `:396-403`) *is* computed correctly as binary nDCG, so the inconsistency is specifically the mislabeled "recall". Rename to `success_at_k`/`hit_at_k`, or compute true recall.

### 5. LOW-MED — Weak artifact identity & size pinning

The gate never validates `kind` / `schema_version` (emitted at `bsard_benchmark.py:432-433` but unchecked), and `corpus_documents`/`questions` are only required to be positive integers (`main.rs:3961-3968`), not pinned to the expected full-BSARD size (~22,633 / 222). Realistically the harness's only truncation path sets the (rejected) limit fields, so harness-generated full artifacts are safe — but defense-in-depth against a hand-edited or unrelated JSON is thin. Pin `kind == "phase1_external_expert_benchmark"` and assert a plausible minimum corpus/question count.

### 6. LOW — Operational / reproducibility

- **No retry/backoff and end-of-run-only caching** (`bsard_benchmark.py:244-262`, `:296-320`): a single transient 429/5xx from OpenRouter after embedding ~22k corpus docs aborts the whole run and discards all embeddings (the `.npz` is written only after both doc and query embedding succeed). Full runs become expensive to restart.
- **No input truncation**: long statutory articles are sent whole; documents exceeding the endpoint's max input tokens will hard-fail a full run. bge-m3 tolerates 8k tokens so most fit, but there's no guard.
- **Cache key omits `base_url`** (`bsard_benchmark.py:265-276`): embeddings from two different endpoints serving the same `model` string collide in cache. The artifact records `base_url_class` but the cache doesn't key on it.
- **Evidence is structurally checked only** — non-empty array of strings (`main.rs:3928-3934`); referenced files are not existence-checked, so "evidence-backed" is weakly enforced.
- **Dense ties not deterministically broken** by doc_id (`bsard_benchmark.py:334-339`), whereas BM25 (`:144`) and hybrid (`:357`) are — minor reproducibility wart.

## What is correctly fail-closed (credit)

The enumerated attack cases all hold: missing env → `pending` (default payload, `main.rs:3799-3801`); unreadable/malformed JSON → `failed` (`:3805-3835`); limited artifacts → rejected via `limit_corpus/limit_questions must be null` (`:3956-3960`, confirmed by the smoke); `state:"passed"` with empty evidence → forced `failed` (`:3928-3934`); wrong `id`/`split`/`jurisdiction`/`usage_scope`/`license` → rejected (`:3935-3945`); any validation error forces `state:"failed"` before `check_status` runs (`:3838-3845`), so a malformed-but-`passed` artifact cannot leak through. The dataset jurisdiction/claim-scope wording correctly frames BSARD as a Belgian French-language proxy, *not* France-LEGI gold, consistent across harness, README, and docs.

## Open questions / risks

- Is the gate intended to be **structural-only** (trust the producer's threshold policy) or to **enforce a quality bar**? Findings #1/#2/#4 are decisive only under the latter — but the stated "fail-closed" intent and "must not treat smoke as valid" requirement read as the latter. Please confirm.
- Should the benchmark be required to use the locked production embedding model (finding #2), or is a "best available model" benchmark acceptable as a scoped external claim?
- BSARD corpus column names (`reference`/`description`/`article`, `bsard_benchmark.py:156-162`) are read best-effort via `.get`; only `id`/`question`/`article_ids` are hard keys. The smoke proves the hard keys exist, but if the optional columns are misnamed, BM25/dense run on near-empty corpus text silently. Worth a one-line assertion that `corpus_text` is non-empty for a sample of docs.

## Verification notes

- Reviewed full diff of `main.rs`, `schema.rs`, `cli_contract.rs`, the new `bsard_benchmark.py`/`README.md`, and the doc/evidence changes. Did not run the suite (read-only review; you reported `cargo test -p jurisearch-cli external_benchmark` / `--test cli_contract`, `cargo fmt`, `py_compile`, `--help`, and the limited OpenRouter smoke all green — consistent with the code).
- Confirmed no threshold floor or metrics-vs-thresholds re-derivation exists anywhere in the gate (validation is `main.rs:3920-3970`; `schema.rs` adds only `type` constraints, no value bounds).
- The two new Rust unit tests and the contract test cover the valid-pass and invalid-pass paths well, but none covers the **valid-metadata + zero-threshold pass** case (finding #1) or the **`revision:"unknown"`** case (finding #3) — add those once fixed.

VERDICT: FIXES_REQUIRED
