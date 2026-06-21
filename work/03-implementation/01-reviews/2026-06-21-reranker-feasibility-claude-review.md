# Claude Review: reranker feasibility

Verdict: GO

Scope reviewed: commit `fbe402f` "Record reranker feasibility spike", covering
`work/03-implementation/02-evidence/2026-06-21-reranker-feasibility.md` and the
0.7 status block added to `work/03-implementation/IMPLEMENTATION_PLAN.md`.

Summary: The spike is a desk/source review plus local machine-capability
inspection, not an empirical benchmark — and it is honest about that. Every
machine fact it cites reproduces independently, the directional conclusions
(HTTP/TEI first, local ONNX/`ort` second, Candle deferred, GPU not assumed) all
follow soundly from the cited evidence and verified hardware, and the plan
records the spike without claiming any code was implemented. No blocking issues;
findings below are accuracy and completeness improvements.

## Findings

- **[Low] Misleading claim — `02-evidence/2026-06-21-reranker-feasibility.md:132`.**
  "Hugging Face cache is effectively empty, so no local reranker weights are
  already present." The cache is not empty: the resolved cache
  (`~/.cache/huggingface` → `/mnt/models/huggingface/hub`) holds ~40+ model
  directories and several datasets (Qwen GGUFs, gemma, PaddleOCR-VL, Qodo-Embed,
  etc.). The *operative* point is correct and is the one worth stating: no
  reranker weights exist locally (`find /mnt/models/huggingface -iname
  '*reranker*'` returns nothing). Non-blocking because the conclusion it drives
  — weights must be fetched before any local run — is unaffected. Recommend
  narrowing to "no reranker weights present; cache otherwise populated with
  unrelated models."

- **[Medium, non-blocking] Task/acceptance deferral not flagged as remaining —
  `IMPLEMENTATION_PLAN.md:403-424` against the 0.7 Tasks/Acceptance at
  `:403-416`.** The 0.7 Tasks list empirical verbs — "Spike local inference
  through `ort` and/or Candle", "Measure top-K reranking latency on fused
  candidate sets", "Verify tokenizer availability and compatibility", "Validate
  packaging implications for local models". None were executed: the evidence doc
  defers latency to a "follow-up benchmark" (`:12`, `:58`, `:97-105`), lists
  tokenizer/packaging as unresolved Risks (`:68-70`), and contains no measured
  numbers. The new "Current status" block lists five `Done:` bullets and zero
  `Remaining:` bullets — unlike 0.6 (`:392`), which explicitly separates a
  `Remaining for Phase 1.2 hardening:` item. As written, a reader scanning the
  status can infer 0.7 is complete, while the literal acceptance "Phase 1 has
  enough data to decide whether reranking can ship **locally**, over HTTP, or
  not at all" (`:414`) is only partially met: HTTP feasibility is reasonably
  established from TEI docs, but the *local* ship decision is explicitly punted
  to a future benchmark. Not blocking — the doc is scoped "feasibility only; no
  reranker adoption decision" (`:4`), the author deliberately did **not**
  annotate the acceptance lines as "Met" (contrast 0.6's "Met in storage
  layer"), and "here is the path + a benchmark matrix to run" is a legitimate
  feasibility outcome. Recommend adding a `Remaining:` bullet, e.g. "latency
  measurement, the `ort`/Candle inference spike, tokenizer/pair-contract
  verification, and packaging tests are deferred to the Phase 1 benchmark," so
  the status cannot be read as 0.7 being empirically complete.

- **[Low] Missing risk: TEI license/operational surface not recorded —
  `02-evidence/2026-06-21-reranker-feasibility.md:11`, `:39`.** TEI is named the
  "first shippable provider," but for a legal-domain product the doc records no
  TEI license/redistribution terms and does not state that an HTTP provider
  requires operating a separate server process (an operational/UX cost the CLI
  otherwise avoids). The latency/operational complexity is acknowledged at
  `:109`, but the license dimension and the "external dependency" tradeoff
  should be on record before "first shippable" is acted on. Non-blocking;
  verify-before-adopt.

- **[Low] Missing risk: `ort` is pre-1.0 — `02-evidence/2026-06-21-reranker-feasibility.md:63`, `:131`.**
  The verified version is `2.0.0-rc.12` (release candidate), correctly cited.
  The doc relegates `ort` to "benchmark, not default," which is the right call,
  but does not flag RC API instability as a risk for the local path. Worth one
  line so the benchmark pins a version and re-checks at promotion time.
  Non-blocking.

## Suggestions

- Add a back-of-envelope CPU latency expectation for a ~560M-param XLM-RoBERTa-
  large-class cross-encoder over 50×1024-token pairs on this CPU. A single rough
  estimate would make "feasible" more concrete before the formal benchmark and
  would sanity-check the `top_n: 50` / `timeout_ms: 30000` sketch at `:49-50`.
- Record rerank-score determinism/reproducibility as a quality gate alongside
  the `:107-111` thresholds — reproducible retrieval is a recurring project
  theme (manifests, stable cursors), and cross-encoder scoring should be
  pinned/deterministic for it.
- State the HTTP-vs-local tradeoff explicitly (HTTP: cheap to build, heavier to
  operate, external dependency; local ONNX: harder to build, self-contained) so
  the W2 eval gate weighs build cost against operational cost, not just quality.
- Cross-link this evidence doc from the W2 eval gate / `:15` ablation note so the
  BM25-only / dense-only / hybrid / hybrid+rerank matrix is tracked where the
  adoption decision will actually be made.

## Verification Notes

- `git show fbe402f --stat` / full diff: confirmed the commit adds only the two
  in-scope files (132 + 8 lines), no code.
- Local machine facts (all reproduce the spike's claims):
  - `lscpu`: "AMD RYZEN AI MAX+ PRO 395 w/ Radeon 8060S"; `Core(s) per socket: 16`,
    `Thread(s) per core: 2`, `CPU(s): 32` → matches "16 cores / 32 threads"
    (`:64`) and "32 logical CPUs" (`:128`); flags include `avx512f avx512dq
    avx512bw avx512vl avx512_vnni avx512_bf16` → confirms "AVX512 flags present".
  - `nvidia-smi`: no device output → confirms `:91`, `:129`.
  - `rocminfo`: `gfx1151` / "AMD Radeon 8060S Graphics" → confirms `:92`, `:130`.
  - `cargo search ort --limit 3`: `ort = "2.0.0-rc.12"` → confirms `:131`.
  - HF cache: `~/.cache/huggingface` → `/mnt/models/huggingface`; `hub/` holds
    ~40+ model dirs + datasets (NOT empty → Finding 1), but
    `find -iname '*reranker*'` is empty → "no local reranker weights" holds.
- Repo state: `grep -rniE 'RerankerConfig|RerankCandidate|RerankScore|fn rerank'`
  and `grep -rniE 'rerank' --include='*.rs'` return nothing; no `ort`/`candle`
  dependency in any `Cargo.toml`. Confirms the plan does not overclaim
  implementation — the `Done:` bullets record decisions/evidence only.
- Integration claims grounded: `grep` confirms the `JURISEARCH_*` env family
  exists (`JURISEARCH_INDEX_DIR`, `JURISEARCH_EMBED_*`, etc.) and the
  "JSON-only stdout, diagnostics on stderr" discipline is a stated CLI invariant
  (`IMPLEMENTATION_PLAN.md:47`, `:160`), so the spike's reuse claims at `:39` are
  real, not aspirational.
- Not verified (no network access this session): the four cited Hugging
  Face/GitHub/docs.rs URLs and the BAAI "8192 max, recommend 1024" discussion
  (`:26`, `:30-31`) were not fetched; treated as plausible and internally
  consistent but unconfirmed.
