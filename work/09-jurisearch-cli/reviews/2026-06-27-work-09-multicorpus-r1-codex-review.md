# Review: work/09 multi-corpus readiness r1

## Findings

No findings.

The staged changes address the reviewed blocker. `stamp_query_readiness` now stamps the active topology as an aggregate: it verifies the just-applied generation is active, iterates every active `corpus_state` row, computes coverage against each corpus's active generation and fingerprint, rejects any incomplete coverage, and stores one stamped report under the aggregate signature (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:340`). The read gates no longer reject `>1` active corpus up front; they still fail closed on no active corpus, missing stamp, malformed stamp, or signature mismatch (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:449`, `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:514`).

The site path now reaches the served multi-corpus topology instead of stopping at readiness. Health reports the multi-corpus topology separately from stamp readiness (`crates/jurisearch-cli/src/site/handlers.rs:211`), and the new site test activates `core` plus `alt` through the visibility path, then proves health readiness, by-id fetch through the union views, and BM25 search through physical fan-out across both corpora (`crates/jurisearch-cli/src/site/tests.rs:597`, `crates/jurisearch-cli/src/site/tests.rs:664`). Existing single-corpus behavior remains covered by the existing site and P3A readiness tests.

Residual risk: the new site e2e exercises BM25, fetch, and health over a two-corpus topology, which matches the blocker. It does not add a site-level dense/hybrid multi-corpus success case, but the existing storage P3C tests still cover multi-corpus fan-out and dense fail-closed fingerprint mismatch, and the reviewed change does not modify that retrieval logic.

## Validation

- `cargo test -p jurisearch-cli site::tests -- --nocapture`
- `cargo test -p jurisearch-storage --test query_readiness_p3a --test query_fanout_p3c -- --nocapture`
- `cargo test -p jurisearch-storage --test generations query_readiness_is_writer_stamped_and_a_not_ready_generation_cannot_activate -- --nocapture`
- `git diff --check --cached`

VERDICT: GO
