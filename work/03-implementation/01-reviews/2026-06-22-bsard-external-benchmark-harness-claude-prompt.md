Please review the current uncommitted diff in /home/pierre/Work/jurisearch.

Scope:
- Add the external BSARD benchmark harness for Phase 1.
- Let `jurisearch status` consume an external benchmark artifact through `JURISEARCH_PHASE1_EXTERNAL_BENCHMARK`.
- Keep the gate fail-closed unless the artifact is a full, valid BSARD test-question run with evidence and required metadata.

Changed files:
- external-benchmarks/README.md
- external-benchmarks/bsard_benchmark.py
- crates/jurisearch-cli/src/main.rs
- crates/jurisearch-core/src/schema.rs
- crates/jurisearch-cli/tests/cli_contract.rs
- work/03-implementation/00-setup/PREREQUISITES.md
- work/03-implementation/IMPLEMENTATION_PLAN.md
- work/03-implementation/02-evidence/2026-06-22-external-expert-benchmark-gate.md
- work/03-implementation/02-evidence/2026-06-22-bsard-external-benchmark-harness-smoke.md

User intent and constraints:
- The benchmark/pseudo-human gate may be external to jurisearch and written in Python.
- It must not promote internal LEGI source-checked fixtures as release-grade.
- It must not treat limited smoke artifacts as valid gate evidence.
- A passing external artifact must be evidence-backed and scoped to an external expert-annotated French-language statutory retrieval benchmark, not France-LEGI human-reviewed gold.
- Do not edit files.

Validation already run:
- `python3 -m py_compile external-benchmarks/bsard_benchmark.py`
- `python3 external-benchmarks/bsard_benchmark.py --help`
- `cargo fmt --all`
- `cargo test -p jurisearch-cli external_benchmark`
- `cargo test -p jurisearch-cli --test cli_contract`
- Limited smoke against OpenRouter `baai/bge-m3`:
  - `--limit-corpus 2500 --limit-questions 3 --embed-batch-size 16`
  - artifact `/tmp/jurisearch-bsard-smoke.json`
  - status rejected it as expected because limits were present:
    - `external_state=failed`
    - `external_check=fail`
    - `claim_allowed=false`
    - error: `dataset.limit_corpus must be null... dataset.limit_questions must be null...`

Please review for:
- Gate safety: can `claim_allowed` open from invalid, limited, failed, missing-evidence, or malformed artifacts?
- Artifact schema and validation: are required fields sufficient, too brittle, or missing?
- Python harness correctness: BSARD splits, qrels parsing, BM25/dense/hybrid scoring, metrics, cache behavior, endpoint/API-key handling.
- Reproducibility: dataset revision pinning, cache key, threshold recording, license/scope metadata.
- Operational risks: full-run time/cost, OpenRouter/hosted endpoint dependence, non-commercial license caveats.

Output structure:
- Findings first, ordered by severity, with file/line references where applicable.
- Open questions/risks.
- Verification notes.
- Final verdict line exactly one of:
  - VERDICT: GO
  - VERDICT: FIXES_REQUIRED
