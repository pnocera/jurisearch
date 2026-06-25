# A2 Candidate Projection Review

## Findings

No findings.

## Verification Notes

- OFF byte-identity holds for the three SQL branches. In `crates/jurisearch-storage/src/retrieval/hybrid.rs:50` and `crates/jurisearch-storage/src/retrieval/hybrid.rs:92`, the placeholder follows the existing `AS snippet,` comma; with `publication_select == ""`, the text collapses exactly to the pre-A2 line and then the existing rank columns. The JSON placeholders at `crates/jurisearch-storage/src/retrieval/hybrid.rs:70` and `crates/jurisearch-storage/src/retrieval/hybrid.rs:123` likewise collapse exactly to the previous `'snippet', snippet,` entry with no extra key or whitespace. The zone path has the same OFF shape at `crates/jurisearch-storage/src/zone_retrieval.rs:249` and `crates/jurisearch-storage/src/zone_retrieval.rs:281`.
- ON projection is syntactically valid in chunk, document, and zone retrieval. `publication_select` adds `d.canonical_json->>'publication' AS publication,` from the joined `documents d` table (`crates/jurisearch-storage/src/retrieval/hybrid.rs:29`, `crates/jurisearch-storage/src/zone_retrieval.rs:230`), and `publication_json` inserts `'publication', publication,` before the next JSON field (`crates/jurisearch-storage/src/retrieval/hybrid.rs:34`, `crates/jurisearch-storage/src/zone_retrieval.rs:235`). The zone path correctly keeps `d` as the document alias even though `source` is selected as `doc_source`.
- Zone mirrors the main path: the fragment strings, column source, empty-OFF behavior, and JSON insertion point match the hybrid path.
- Construction-site coverage is complete. `rg "(HybridCandidateQuery|ZoneCandidateQuery)\\s*\\{" crates/jurisearch-cli crates/jurisearch-storage` shows all direct literals updated; the `ZoneCandidateQuery` spread literals in `crates/jurisearch-storage/tests/zone_units.rs:441`, `:454`, `:470`, and `:479` inherit `project_authority: false` from `base`.
- Field placement is sane. `HybridCandidateQuery.project_authority` is in the query struct at `crates/jurisearch-storage/src/retrieval/types.rs:88`; `ZoneCandidateQuery.project_authority` is in the zone query struct at `crates/jurisearch-storage/src/zone_retrieval.rs:41`. No `project_authority` field landed in `SearchExecution`, `DecisionFilters`, or other inner literals.
- The A2 assertions in `crates/jurisearch-storage/tests/decision_projection.rs:163` and `crates/jurisearch-storage/tests/decision_projection.rs:180` lock the OFF no-key behavior and the ON `"oui"` projection for the reused query. The `..decision_query` update is correct for this copyable query value.

## Validation

- `cargo test -p jurisearch-storage --test decision_projection --test retrieval_smoke --test zone_units` passed: 16 tests.
- `cargo check -p jurisearch-storage --tests` passed.
- `cargo check -p jurisearch-cli` passed.

VERDICT: GO
