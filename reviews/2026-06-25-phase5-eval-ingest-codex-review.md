# Codex Review: Phase 5 Eval/Ingest Split

Scope reviewed:

- `git diff 4d13165..HEAD -- crates/jurisearch-cli`
- Commits `75e7b80`, `7c74d95`, and `612a8e1`
- Eval split under `crates/jurisearch-cli/src/eval/*`
- Ingest split under `crates/jurisearch-cli/src/ingest.rs` and `crates/jurisearch-cli/src/ingest/*`

## Findings

No findings.

## Review Notes

- `emit_eval` dispatch remains wired to the same payload builders and artifact emitters. CodeGraph shows the expected callees: `eval_phase1_payload`, `eval_france_legi_payload`, `eval_france_juris_payload`, `eval_france_juris_zones_payload`, `eval_run_payload`, `eval_tune_payload`, `write_json`, `emit_artifact`, and `emit_error`.
- `emit_ingest` and `sync_payload` remain wired to the expected ingest payloads: LEGI archives, JURI archives, chunk embedding, zone enrichment, zone-unit build/embed, legislation citation collection/enrichment, and LEGI hierarchy backfill.
- I compared the current moved function bodies against `4d13165:crates/jurisearch-cli/src/main.rs` for the behavior-critical paths. The reviewed bodies were byte-identical after extraction, including:
  - `emit_eval`
  - `emit_ingest`
  - `sync_payload`
  - `ingest_legi_archives_payload`
  - `ingest_juri_archives_payload`
  - `process_legi_archive_member`
  - `process_juri_archive_member`
  - `maybe_quarantine_payload`
  - `normalize_since`
  - `default_juri_run_id`
  - `embed_chunks_payload`
  - `enrich_zones_payload`
  - `build_zone_units_payload`
  - `embed_zone_units_payload`
  - `eval_france_legi_payload`
  - `eval_france_juris_payload`
  - `eval_france_juris_zones_payload`
  - `eval_tune_payload`
  - `eval_phase1_payload`
- The eval metric artifact helpers still floor, not round, gate metrics via `floor_metric`.
- The ingest archive paths preserve run-id derivation, selected archive ordering, since filtering, per-member resume/compatibility handling, parse-error classification, quarantine writes, final manifest/run-status handling, and replay-snapshot refresh behavior.
- The zone-unit pipeline preserves the concurrency-bounded enrichment, official/fallback/error accounting, derivation from cached `decision_zones`, and separate zone-unit dense rebuild path.
- I did not rerun `cargo build` or `cargo test` because this review request explicitly limited file modifications to the saved Markdown review; cargo would write build artifacts. The review brief reports `cargo build -p jurisearch-cli` and `cargo test -p jurisearch-cli` already passed.

VERDICT: GO
