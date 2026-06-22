Findings

WARN crates/jurisearch-cli/src/main.rs:4261 - `provenance.source_revision` only rejects the exact string `unknown`. Because the earlier required-field check trims only for emptiness, a gate artifact with `source_revision` set to ` unknown ` or `UNKNOWN` still validates and can pass if the metrics and other provenance fields are present. That leaves a small but real unpinned-revision bypass in the hardening this change is meant to add. Fix: normalize before the unknown comparison, for example `artifact_pointer_str(artifact, "provenance.source_revision").is_some_and(|value| value.trim().eq_ignore_ascii_case("unknown"))`, and add a regression case for whitespace/case variants.

Review notes

- The hard boolean checks use `artifact_pointer_value(...).and_then(Value::as_bool) != Some(false)`, so `sampled=true`, `human_in_gold=true`, `llm_in_gold=true`, missing flags, nulls, and string `"false"` all fail.
- Missing or blank `provenance.official_source`, `provenance.source_revision`, `provenance.pipeline`, `provenance.code_version`, and `provenance.index_revision` fail. The gap is only the non-normalized `unknown` comparison above.
- `valid_france_legi_artifact()` satisfies the new provenance, metric, threshold, embedding, jurisdiction, and evidence rules.
- `payload["provenance"]`, `categories`, and `thresholds` are surfaced from the artifact consistently with the BSARD gate's dataset/metrics/thresholds surfacing pattern.
- `FranceLegiGate.provenance` matches the emitted top-level shape as object-or-null.

VERDICT: FIXES_REQUIRED
