# P2 client storage topology re-review

## BLOCKER

### CLI query-readiness still bypasses the active generation

The r1 production-read blocker is only partially fixed. The storage retrieval helpers now call `execute_read_sql`, but the real CLI query path gates every read before those helpers run: `crates/jurisearch-cli/src/index_runtime.rs:48` opens the index and `crates/jurisearch-cli/src/index_runtime.rs:54` calls `ensure_query_readiness`. That path calls `load_or_compute_query_readiness` at `crates/jurisearch-cli/src/index_runtime.rs:104`, which opens a raw `postgres::Client` at `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:119` and computes coverage from unqualified `documents`/`chunks`/`chunk_embeddings` at `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:164` and `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:194`. With the default search path, those are still `public` tables.

That means a client with `corpus_state` pointing at `jurisearch_server_core_g0001` can have fully query-ready generation tables while `public` is empty or stale, and `search`/`fetch`/`context`/`related`/`cite` can fail the readiness gate before the new read-role SQL is reached. The new regression at `crates/jurisearch-storage/tests/generations.rs:287` calls `fetch_documents_json` and `context_documents_json` directly, so it does not exercise `open_query_index` or the readiness gate that the CLI actually uses. The cache makes the opposite failure possible too: a stale `index_manifest.query_readiness` row from `public` can let a generation-backed client pass without checking the active generation's coverage.

Fix: make query-readiness a client-read-role operation as well. Either compute the coverage queries through the same resolved search path as `execute_read_sql`, or resolve the active physical schema and qualify the coverage queries explicitly. The readiness cache also needs to be keyed by, or invalidated on, the active generation/sequence so a report for `public` or a previous generation cannot authorize the current one. Add a CLI-level regression that activates a generation, makes `public` empty/stale, and proves `open_query_index` plus a real fetch/search command still passes readiness and reads the generation.

### `expected_previous_sequence = None` still bypasses the cursor guard

`activate_generation` only checks the cursor when `expected_previous_sequence` is `Some` (`crates/jurisearch-storage/src/generations.rs:374`). Passing `None` skips the `corpus_state` lookup entirely, then the function retires the old active generation and upserts the cursor at `crates/jurisearch-storage/src/generations.rs:391` through `crates/jurisearch-storage/src/generations.rs:426`. The instructions say `None` is for the first baseline, but the implementation accepts it even when a corpus already has an active cursor.

A stale or miswired caller can therefore activate `core_g0002` with `None` after `core_g0001` is active, bypassing the §7.3 expected-previous-sequence guard and clobbering the cursor. The transaction atomicity fix is real, but the cursor validation part of the r1 blocker is not fully resolved.

Fix: always select `jurisearch_control.corpus_state` for the corpus `FOR UPDATE`. If `expected_previous_sequence` is `None`, require that no row exists; if it is `Some(n)`, require `sequence = n`. Add a regression that activates g1, creates g2, calls `activate_generation(..., None)`, and asserts the switch is rejected and `corpus_state`/registry remain on g1.

## WARN

### France benchmark gold/revision reads still use `public`

Several CLI eval read surfaces still bypass the read role. `france_juris_gold_json` uses `postgres.execute_sql` for all qrel extraction at `crates/jurisearch-storage/src/france_juris.rs:54`, `crates/jurisearch-storage/src/france_juris.rs:59`, and `crates/jurisearch-storage/src/france_juris.rs:60`; `france_juris_zone_gold_json` does the same at `crates/jurisearch-storage/src/france_juris.rs:206`; `france_legi_gold_json` does it at `crates/jurisearch-storage/src/france_legi.rs:50`; and `france_juris_index_revision` still counts `documents`/`chunks`/`chunk_embeddings` through `public` at `crates/jurisearch-storage/src/france_juris.rs:269`.

If these commands are run against a client index whose replicated corpus lives only in an active generation, the benchmark gold and revision can be generated from stale or empty `public` data while the retrieval side reads the generation. That creates misleading eval artifacts and can hide topology regressions outside the `retrieval/*` modules.

Fix: route the corpus-data portions of the France qrel/revision helpers through the same client read role, while leaving genuinely global/control reads such as `index_manifest` or `ingest_run` intentionally in `public`. Add a generation-backed eval regression with stale `public` to prove the qrels/revision follow the active generation.

## Resolved r1 items

The direct retrieval helpers in `retrieval/*` and `zone_retrieval.rs` now call `execute_read_sql`; activation now owns a single switch transaction; `drop_retired_generation` verifies a retired registry row before dropping; generation creation is single-use; migration v20 creates empty stable views; and the two r1 nits are addressed.

Validation was not run; this was a review-only pass.

VERDICT: FIXES_REQUIRED
