# Codex Review r3 - Phase 2.6 Evaluation Gate

Reviewed change: `4edc7f7` (`git diff HEAD~1 HEAD`)

## BLOCKER

None.

## WARN

1. `Phase2BenchmarkGate.artifact` still does not match every emitted benchmark payload.

   Evidence: `phase2_benchmark_payload_with_path` copies the parsed benchmark JSON into `payload["artifact"]` before validating the artifact shape (`crates/jurisearch-cli/src/main.rs:8133`, `crates/jurisearch-cli/src/main.rs:8144`). The new schema entry declares that field as only `object | null` (`crates/jurisearch-core/src/schema.rs:514`). A benchmark file containing valid JSON that is not an object, such as `[]` or `false`, will parse successfully, be re-derived as `failed`, and still be emitted verbatim as `artifact: []` or `artifact: false`, which violates the public schema.

   Impact: the r2 schema mismatch is fixed for normal object artifacts and non-string `artifact_reported_state`, but parseable malformed artifacts can still produce a status payload that consumers cannot validate against the published schema.

   Concrete fix: either normalize `payload["artifact"]` to an object-or-null diagnostic before emission, or loosen the schema for `artifact` to accept any JSON value. Add a negative test with a parseable non-object artifact so the schema and payload contract stay aligned.

## NIT

1. The provenance comment still contains the contradiction called out in r2.

   Evidence: the old sentence remains at `crates/jurisearch-cli/src/main.rs:8211`: "the benchmark must run through the production pipeline, with pinned code/index revisions and no sampling/human/LLM gold." The validator immediately below still allows human/LLM gold as long as `human_in_gold` and `llm_in_gold` are booleans (`crates/jurisearch-cli/src/main.rs:8224`), and the valid fixture keeps `llm_in_gold: true` (`crates/jurisearch-cli/src/main.rs:9396`). The new clarification helps, but it does not remove the contradictory sentence.

   Concrete fix: replace the old sentence with the intended policy, for example: "Production provenance: the benchmark must run through the production pipeline, with pinned code/index revisions, `sampled=false`, and disclosed human/LLM gold booleans."

## Verification

- `git diff --check HEAD~1 HEAD` passed.
- `cargo test -p jurisearch-cli phase2_` passed: 5 tests.
- `cargo test -p jurisearch-cli --test cli_contract status_returns_json_without_index` passed.
- `cargo test -p jurisearch-core schema` passed.

VERDICT: FIXES_REQUIRED
