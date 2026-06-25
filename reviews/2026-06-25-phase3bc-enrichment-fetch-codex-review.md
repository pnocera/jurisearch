# Code Review: Phase 3b/3c Enrichment + Fetch Extraction

Scope reviewed: `git diff 292805c..HEAD -- crates/jurisearch-cli`

## Findings

No BLOCKER, WARN, or NIT findings.

## Review Notes

- The `fetch` command body moved to `crates/jurisearch-cli/src/retrieval/fetch.rs` with the same payload flow: parse `--part`, require/open the index, enforce `QueryReadinessGate::Fetch`, call `fetch_documents_json`, map an empty document response to the same `no_results` error, and apply `annotate_fetched_parts` only after the base fetch payload is materialized.
- `FetchDocumentsQuery` and `fetch_documents_json` are imported locally in `retrieval/fetch.rs`, preserving behavior while avoiding the old root-level unused import.
- The enrichment helpers are split across `archive`, `decision_part`, `judilibre_zones`, and `legislation`, then re-exported through `enrichment/mod.rs`. The moved function bodies match the prior `main.rs` implementation aside from required `pub(crate)` visibility and module placement.
- The shared archive helpers are centralized in `enrichment/archive.rs` and used by both Judilibre zone enrichment and Legifrance citation enrichment, with the same `sha256:<hex>` hashing, archive hard-error behavior, and local unsupported accounting row.
- The Judilibre zone path preserves the cache decision rules: fresh `ok` rows serve official fragments, fresh negative rows fall back without network, missing/expired rows enrich only for online Cassation sources, and upstream/unsupported/not-found outcomes are cached with the same TTL policy.
- The legislation citation collection/enrichment path preserves the parser, citation key derivation, archived `/decision` visa scan, Legifrance search archiving, resolution status updates, cursor handling, and summary accounting.

## Validation

- Source/diff inspection against `292805c..HEAD` for `crates/jurisearch-cli`.
- `git diff --check 292805c..HEAD -- crates/jurisearch-cli` passed.
- `cargo test -p jurisearch-cli --no-run` passed.

VERDICT: GO
