# Code Review: Zone Z3

## Scope

Reviewed commit `e2bc5baab7270340515c0bffecb815dbfca7340f` (`Zone retrieval Z3: build-zone-units + embed-zone-units`) against `origin/main` (`1781caaf4bcbeb6925893238833e208a8d69dcae`).

The review focused on:

- `crates/jurisearch-cli/src/main.rs`
  - `ingest build-zone-units`
  - `ingest embed-zone-units`
  - `derive_zone_unit_rows`
  - `embed_and_insert_with_pool` and the chunk/zone wrapper split
- Existing storage contracts in `crates/jurisearch-storage/src/zone_units.rs`
  - derivable-row selection
  - zone-unit replacement
  - zone-unit embedding upsert
  - zone dense finalization
- The added previous-Z2 review artifact for scope/metadata impact

## Findings

No findings.

`build-zone-units` stays within the Z2/Z3 storage contract: it reads only fresh `ok` decision-zone rows with non-null `text_hash`, relies on the storage query for Cassation/INCA resolver scope, derives one row per non-empty official fragment, and writes each decision through `replace_zone_units_for_document`, which transactionally deletes and reinserts only that document's zone units. Empty fragments are skipped with contiguous per-zone `fragment_index` values, and the new unit test covers the multi-fragment/blank-skip case.

`embed-zone-units` mirrors the existing chunk embedding flow against the parallel zone tables: it validates the same embedding dimension/fingerprint shape, refuses explicit `--limit` runs that would leave pending zone units, streams unbounded production runs in bounded pages, upserts via the same embedding pool driver, and finalizes only after `finalize_zone_dense_rebuild` confirms complete coverage for the requested fingerprint/model/dimension. The generic pool refactor preserves the chunk insertion path as an injected storage step; the existing `insert_chunk_embeddings` guard shape is preserved by the new `insert_zone_unit_embeddings` path.

The storage-level checks remain aligned with the migrations: `zone_units` IDs and uniqueness match the derived `(document_id, zone, fragment_index)` rows; embeddings cascade on unit replacement; the zone dense index and manifest are isolated under `zone_unit_embeddings_ivfflat_idx` and `index_manifest['zone_embedding']`, so the existing whole-decision dense path is not modified.

## Verification

- `git status --short`
- `git log --oneline --decorate -n 12`
- `git diff --stat origin/main..HEAD`
- `git diff --name-status origin/main..HEAD`
- `git diff --check origin/main..HEAD`
- CodeGraph context/explore for `build_zone_units_payload`, `derive_zone_unit_rows`, `embed_zone_units_payload`, and the zone-unit storage APIs
- `cargo test -p jurisearch-cli derive_zone_unit_rows_handles_multi_fragment_and_skips_empty`
- `cargo test -p jurisearch-storage zone_unit`
- `cargo test -p jurisearch-cli`
- `cargo test -p jurisearch-storage --test zone_units`

VERDICT: GO
