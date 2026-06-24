# Code Review: Phase 2 France Juris Benchmark

Reviewed the uncommitted diff for:

- `crates/jurisearch-storage/src/france_juris.rs`
- `crates/jurisearch-storage/src/lib.rs`
- `crates/jurisearch-cli/src/main.rs`

I checked the Phase 2 gate contract in `phase2_benchmark_artifact_errors`, the production retrieval/citation paths (`search_with_postgres`, `citation_lookup_json`, `parse_citation_target`), the decision storage/projection schema, and the gold extraction methodology. I did not run `eval france-juris` or mutate the index.

## Findings

### WARN: `compare --kind decision` returns a misleading top-level `kind`

`compare_payload` now accepts and applies `LegalKind::Decision` via `kind_filter = Some("decision")`, but the response still serializes every non-code request as `"kind": "all"`:

```rust
"kind": if matches!(kind, LegalKind::Code) { "code" } else { "all" },
```

That means `jurisearch compare --kind decision ...` will correctly filter to decisions, but the machine-readable response says it compared the full corpus. This is a contract honesty bug in the newly enabled decision compare surface, and it will confuse downstream audit artifacts even if the retrieval itself is filtered.

Concrete fix: add a small formatter for `LegalKind` or match all three variants at `crates/jurisearch-cli/src/main.rs:3333`:

```rust
"kind": match kind {
    LegalKind::Code => "code",
    LegalKind::Decision => "decision",
    LegalKind::All => "all",
},
```

### WARN: Retrieval qrel caps can underfill because filtering happens after `LIMIT`

`retrieval_sql` selects the first `limit` candidate summaries by `document_id`, then applies the cleaned-query length filter outside that limited subquery:

```sql
ORDER BY d.document_id
LIMIT {limit}
) q
WHERE length(btrim(query)) >= 60
```

If early `decision_summary` chunks become too short after identifier stripping, the category can return fewer qrels than requested even when plenty of later valid qrels exist. The artifact will fail closed if this drops below the gate floors, so this is not a paper-pass risk, but it can make the benchmark brittle and unnecessarily fail before measuring the available corpus.

Concrete fix: compute `query` in an inner CTE/subquery, filter `length(btrim(query)) >= 60`, then apply `ORDER BY document_id LIMIT {limit}` after the filter. That preserves deterministic selection while filling the requested cap when enough valid summaries exist.

### NIT: Add focused regression coverage for the new artifact builder and decision compare response

The existing Phase 2 gate tests validate artifact consumption, and citation decision tests cover the resolver path, but this diff adds a new producer (`france_juris_artifact` / `eval france-juris`) and changes `compare --kind decision` behavior without a focused regression around either surface.

Concrete fix: add a unit-level test that constructs `FranceJurisCategoryResult`s and verifies the produced artifact re-derives as passed/failed through `phase2_benchmark_payload_with_path`, plus a CLI contract test that `compare --kind decision` both filters to decision documents and reports `"kind": "decision"`.

## Notes

The benchmark artifact shape matches the current gate contract: jurisdiction, fingerprint, non-empty evidence, production provenance, boolean gold disclosure, both retrieval categories, and per-identifier ECLI/pourvoi/CETATEXT citation metrics are present. The runner measures retrieval through `search_with_postgres` with `LegalKind::Decision`, document grouping, hybrid mode, and a shared embedder. Citation qrels go through `parse_citation_target` and `citation_lookup_json`, matching the local resolver used by `cite`.

The gold extraction is deterministic and source-grounded in indexed official fields (`decision_summary`, `canonical_json.ecli`, `canonical_json.case_numbers`, `source_uid`). I did not find fabricated metrics or a gate-shape mismatch.

VERDICT: GO
