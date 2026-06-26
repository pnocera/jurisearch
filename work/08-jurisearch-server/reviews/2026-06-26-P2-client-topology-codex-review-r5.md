# Code Review: P2 Client Storage Topology r5

## Findings

No findings.

## Confirmed

- The r4 blocker is resolved. The client-facing `fetch --part --online` path now resolves Judilibre lookup metadata before opening the raw write connection: `enrich_decision_from_judilibre` calls `decision_resolution_metadata_json` and passes the parsed metadata into `enrich_decision_from_judilibre_with_client` (`crates/jurisearch-cli/src/enrichment/judilibre_zones.rs:91-110`).
- The client metadata helper now uses the read role. `decision_resolution_metadata_json` runs through `ManagedPostgres::execute_read_sql`, so `documents` resolves to the active generation for a generation-backed client (`crates/jurisearch-storage/src/decision_zones.rs:55-82`).
- The producer path still resolves metadata on its own worker connection and writes on that same caller-owned connection. `enrich_zone_page_concurrently` calls `decision_resolution_metadata_with_client(&mut db, doc_id)`, parses the JSON, and passes it to the shared enrichment core (`crates/jurisearch-cli/src/ingest/pipeline.rs:159-179`). The shared core still wraps archive/cache mutations in `in_outbox_txn`, so the outbox/transaction coupling is preserved (`crates/jurisearch-cli/src/enrichment/judilibre_zones.rs:132-165`, `crates/jurisearch-cli/src/enrichment/judilibre_zones.rs:180-263`).
- The decision-zone read entry points are now on the intended side of the boundary: `decision_zones_json` and `decision_resolution_metadata_json` use `execute_read_sql`, while `decision_resolution_metadata_with_client` remains the producer/backfill variant and has only the producer caller in the current call graph.
- I did not find another client-facing read surface still resolving replicated data from stale `public`. The expected retrieval, citation, France eval/gold, stats/status coverage, zone retrieval, decision-zone cache lookup, and Judilibre metadata surfaces now route through `execute_read_sql` or the matching read-role search path. The remaining direct `execute_sql`/raw-client replicated-table reads I found are producer/enrichment/build paths, migrations, or test/setup helpers.
- The new CLI regression covers the split-brain trigger that mattered for r4: it activates a generation carrying the parser-valid pourvoi/ECLI, makes `public.documents` stale by stripping `case_numbers`, and runs the real `jurisearch fetch cass:DEC1 --part motivations --online` path. That is sufficient for this P2 boundary because the online cache write-back still targets `public` and `decision_zones.document_id` still needs the public FK row; an empty-public variant would require moving client cache writes into a generation/app-local home, which is outside the accepted P2 boundary.

## Validation

I did not run the test suite for this pass; this was an independent static review of the current working tree, the r4 diff, and the relevant call graph.

VERDICT: GO
