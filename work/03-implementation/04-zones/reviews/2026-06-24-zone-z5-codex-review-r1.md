# Code Review - Zone Retrieval Z5

Reviewed commit `b87ac8c` (`Zone retrieval Z5: status.zone_retrieval + zone eval benchmark`) against the Z5 plan and the review-gate constraints.

## Findings

### WARN: Zone gold can still leak Cassation pourvoi identifiers

The new zone gold SQL strips ECLI/JURITEXT/CETATEXT identifiers from `zone_units.body`, but it does not strip Cassation pourvoi-shaped identifiers. This benchmark is specifically scoped to official Cassation zones (`cass+inca`), and pourvoi numbers are production-supported decision identifiers elsewhere in the same benchmark family. If an official zone fragment contains text like `pourvoi n 12-34567`, `n° 12-34567`, or a bare `12-34567`, the qrel query can become a lookup-style identifier query rather than an identifier-stripped semantic excerpt, violating the T5.2 "leak-free gold" constraint.

Location: `crates/jurisearch-storage/src/france_juris.rs:236-239`

Recommended fix: extend the stripping expression used by `zone_retrieval_sql` to remove parser-valid pourvoi/case-number forms that can identify a Cassation decision, preferably reusing the normalized pourvoi shape already used by the resolver (`NN-NNNN..`) and covering common official text prefixes such as `pourvoi`, `n°`, `no`, and spacing/dot variants. Add a storage test that seeds a zone fragment containing both the decision's pourvoi and semantic text, then asserts the emitted query keeps the semantic text but omits the pourvoi.

### WARN: The zone benchmark artifact hardcodes the embedding fingerprint even when it was not used

`eval france-juris-zones` permits `--mode bm25`, `--mode dense`, and `--mode hybrid`, and it only prepares embeddings when the mode uses dense retrieval. However the emitted `phase2_zone_benchmark` artifact always records `"fingerprint": "bge-m3:1024:normalize:true"`. For `--mode bm25`, that fingerprint is not used at all; for dense/hybrid under a different configured embedder, readiness checks the configured fingerprint, while the artifact still claims bge-m3. That makes the measured artifact's provenance untrustworthy even though the benchmark is intentionally measured-only.

Location: `crates/jurisearch-cli/src/main.rs:2780-2786`, `crates/jurisearch-cli/src/main.rs:2973-2980`

Recommended fix: carry the actual retrieval fingerprint into `zone_benchmark_artifact`: for dense/hybrid, record the same fingerprint used by `ensure_zone_retrieval_readiness` or the finalized `zone_embedding` manifest; for BM25, record `null`/`not_applicable` or omit the dense fingerprint field and add a separate `uses_dense: false` field. Add a CLI unit test or artifact-construction test covering at least BM25 mode so the artifact cannot claim a dense fingerprint for a lexical-only run.

## Notes

- The status block is isolated from the Phase 2 gate: `status.zone_retrieval` is added as a separate top-level payload field, while `phase2_gate_payload` still consumes only corpus source status plus the Phase 2 jurisprudence benchmark.
- The resolver-reachable denominator uses the same `PARSER_VALID_POURVOI_EXISTS` predicate as `enrich_zone_candidates_json` and is only called from `status`, not the zone-search hot path.
- The zone eval runner calls `zone_candidates_json`, scopes each category by the requested zone, checks returned candidates for `zone_accurate=true`, and reports empty categories as `value: null` rather than a misleading `0.0`.
- The new `phase2_zone_benchmark` artifact uses a distinct `kind` and `gate_input: false`; it cannot satisfy the existing Phase 2 gate validator because that validator still requires the production Phase 2 categories and provenance.

VERDICT: FIXES_REQUIRED
