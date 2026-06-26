# P2 client storage topology re-review r3

## BLOCKER

### CLI `cite` and France decision-citation scoring still read `public`

The r2 read-topology fixes cover fetch/search/readiness and the France gold/revision builders, but the production citation lookup path is still outside the client read role. `cite_payload` opens a query index and passes the readiness gate at `crates/jurisearch-cli/src/retrieval/cite.rs:22`, then calls `citation_lookup_json` at `crates/jurisearch-cli/src/retrieval/cite.rs:23`. That storage helper still executes the assembled lookup with `postgres.execute_sql` at `crates/jurisearch-storage/src/citation.rs:103`, while the lookup SQL reads the replicated corpus tables `documents` and `legi_metadata_roots` at `crates/jurisearch-storage/src/citation.rs:183` and `crates/jurisearch-storage/src/citation.rs:211`.

That leaves a client with `corpus_state` pointing at an active generation in a split-brain state: `ensure_query_readiness` now measures the generation, `fetch`/`search` read the generation, but `jurisearch cite ...` resolves identifiers against stale or empty `public`. If the same setup used by the new fetch regression empties `public` after activation, the citation command would pass readiness and then return empty matches / `not_found` for a document that is present in the served generation.

This also keeps the France-juris benchmark inconsistent. The ecli/pourvoi/cetatext gold qrels now come from the active generation via `france_juris_gold_json` at `crates/jurisearch-storage/src/france_juris.rs:63`, `crates/jurisearch-storage/src/france_juris.rs:64`, and `crates/jurisearch-storage/src/france_juris.rs:65`, but the scoring path calls `france_juris_cite_documents` -> `citation_lookup_json` at `crates/jurisearch-cli/src/eval/france_juris.rs:185`. So the benchmark can generate identifier qrels from the generation and score those identifiers against stale `public`.

Fix: make `citation_lookup_json` execute through `postgres.execute_read_sql`. Its corpus reads are all replicated tables, and the `jurisearch_normalized_case_numbers` function used by decision pourvoi lookup will still resolve through the `public` fallback under `generation, public`. Add a CLI-level regression that activates a generation, empties `public`, runs the real `jurisearch cite <document id or ECLI>` command, and asserts the served generation document is matched. Add a France decision-citation regression or extend the existing France generation test so ecli/pourvoi/cetatext scoring also proves generation reads.

## Resolved r2 checks

- `apply_read_search_path` mirrors `execute_read_sql`'s 0/1/>1 topology arms and quotes schema names with `sql_identifier` (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:50` through `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:73`).
- Query-readiness cache hits are scoped by `active_read_signature`, including `corpus:active_generation:sequence`, and `load_or_compute_query_readiness` rejects a cache whose embedded signature differs from the current topology (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:32` through `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:43`, `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:207` through `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:216`).
- `activate_generation` now always locks `corpus_state` and rejects `(expected_previous_sequence = None, current_sequence = Some(_))`, so `None` is limited to the first baseline (`crates/jurisearch-storage/src/generations.rs:401` through `crates/jurisearch-storage/src/generations.rs:418`).
- France gold and revision helpers now use `execute_read_sql`; the revision digest's global `index_manifest` / `ingest_run` subqueries correctly fall through to `public` because those tables are not per-generation (`crates/jurisearch-storage/src/france_juris.rs:57` through `crates/jurisearch-storage/src/france_juris.rs:65`, `crates/jurisearch-storage/src/france_juris.rs:278` through `crates/jurisearch-storage/src/france_juris.rs:299`, `crates/jurisearch-storage/src/france_legi.rs:52` through `crates/jurisearch-storage/src/france_legi.rs:55`).

## Validation

- `cargo test -p jurisearch-storage --test generations activating_with_none_against_an_installed_corpus_is_rejected`
- `cargo test -p jurisearch-storage --test generations query_readiness_resolves_to_the_active_generation_and_a_public_cache_cannot_authorize_it`
- `cargo test -p jurisearch-cli --test cli_retrieval_contract fetch_passes_readiness_and_reads_the_active_generation_not_stale_public`
- `cargo test -p jurisearch-storage --test generations france_eval_gold_and_revision_follow_the_active_generation_not_stale_public`

VERDICT: FIXES_REQUIRED
