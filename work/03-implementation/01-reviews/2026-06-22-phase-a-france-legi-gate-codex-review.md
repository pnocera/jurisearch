# Review: Phase A France-LEGI gate

## Findings

BLOCKER: None found.

WARN: [crates/jurisearch-cli/src/main.rs:5330](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:5330) `phase1_gate_payload_maps_ready_inputs_and_failed_members` now asserts that `france_legi_official_eval` is pending, but the function under test reads `JURISEARCH_PHASE1_FRANCE_LEGI_BENCHMARK` from the ambient process environment. This makes the test fail on a developer/CI run where that env var points at a valid artifact. The existing BSARD check already has the same ambient-env risk; this change adds the new env-dependent assertion. Fix: refactor the gate builder so tests can pass prebuilt benchmark payloads, or isolate both benchmark env vars under a serialized env guard that restores prior values.

WARN: [crates/jurisearch-cli/src/main.rs:4226](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:4226) The artifact validation only requires non-empty `source` and `retriever` strings. That is enough to mirror the current free-form BSARD metadata style, but it does not enforce the France-LEGI design claim that gold came from official DILA/Legifrance fields and that retrieval ran through the production `jurisearch search` pipeline. A proxy runner artifact can pass if it supplies good-looking category metrics, query counts, and embedding metadata. Fix: before relying on this as release evidence, make these provenance fields structured or enumerated, for example requiring official source/archive or API revision, production pipeline identifier/commit/index revision, and no sampled/truncated qrel flags.

NIT: [crates/jurisearch-cli/src/main.rs:3778](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:3778) The check message says Phase 1 "should clear" the France-LEGI benchmark, while the implementation adds it to the all-checks release gate. Fix: say "requires" to match the actual blocker semantics and avoid ambiguity in status output.

## Review Notes

The France-LEGI artifact path mirrors the BSARD machinery closely: `phase1_france_legi_payload_with_path` loads the env-pointed JSON, copies artifact evidence/categories/thresholds into the status payload, runs validation, and overwrites payload state to `failed` when validation errors exist. A self-reported `state: "passed"` does not bypass below-floor category metrics, missing category metrics, wrong jurisdiction, wrong kind/schema version, wrong locked embedding model/dimension/normalization, empty evidence, or too-few category queries.

`phase1_france_legi_validate_category` enforces both sides of the policy: artifact thresholds must be at or above the hard-coded floor, and observed metric values must be at least the artifact threshold. Missing category paths produce required-field errors, so the three required categories cannot silently disappear.

`phase1_france_legi_check_status` is intentionally shallow like `phase1_external_benchmark_check_status`; it trusts the already-normalized payload state and only adds the non-empty evidence requirement before returning `pass`. In the actual gate path, that normalized state has already been derived by `phase1_france_legi_artifact_errors`, so the listed below-floor/missing-category/wrong-fingerprint cases do not pass.

Additivity is safe: [crates/jurisearch-cli/src/main.rs:3775](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:3775) inserts `france_legi_official_eval` into the same `checks` vector, and [crates/jurisearch-cli/src/main.rs:3799](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/main.rs:3799) still computes `claim_allowed` as every check being `pass`. With no France-LEGI env var, the new check is pending and `claim_allowed=false`; with a failing artifact, it is fail and `claim_allowed=false`.

Schema consistency looks correct for the current emitted payload: [crates/jurisearch-core/src/schema.rs:414](/home/pierre/Work/jurisearch/crates/jurisearch-core/src/schema.rs:414) adds `france_legi_benchmark`, and [crates/jurisearch-core/src/schema.rs:447](/home/pierre/Work/jurisearch/crates/jurisearch-core/src/schema.rs:447) defines fields matching `phase1_france_legi_default_payload` plus artifact-derived categories/thresholds/evidence.

The new tests cover the main gate logic: valid artifact consumption, no-path pending behavior, rejection of low metrics/wrong jurisdiction/too-small categories/missing cross-reference, and the evidence requirement in `phase1_france_legi_check_status`. I would add follow-up assertions for wrong embedding fingerprint/dimension/normalization and for the ambient-env isolation noted above, but the current tests are adequate for the core metric-floor behavior.

Design answer: additively requiring both `external_expert_annotated_eval` and `france_legi_official_eval` is acceptable for this Phase A wiring because it is conservative and matches the stated additive scope. It intentionally keeps `claim_allowed=false` until both gates pass. BSARD should still be demoted to optional robustness evidence before any Phase 1 release claim depends on the new France-LEGI gate; doing that in a separate change is cleaner than mixing the release-policy demotion into this wiring diff.

Verification: inspected `git diff` for `crates/jurisearch-cli/src/main.rs` and `crates/jurisearch-core/src/schema.rs`; `git diff --check -- crates/jurisearch-cli/src/main.rs crates/jurisearch-core/src/schema.rs` is clean. I did not rerun `cargo test -p jurisearch-cli`, per the review note that it is being run separately.

VERDICT: GO
