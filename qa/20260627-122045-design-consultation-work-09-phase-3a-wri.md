# Phase 3A Design Consultation

**Verdict: GO-with-adjustments.**

The direction is right: move readiness from a query-time cache into a writer-owned stamp, stamp both activation and incremental commits, and add a fail-closed dense fingerprint preflight. The adjustments are mostly boundary details: do not make `signature == "public"` the only authority for legacy compute-on-read, make the stamp helper usable from a transaction without `search_path`, and put the fingerprint preflight in storage so P4 inherits it.

Current-source facts behind the verdict:

- `load_or_compute_query_readiness` still writes `index_manifest['query_readiness']` on a query-time cache miss.
- The active-read signature is already `corpus:active_generation:sequence` or `public`; incrementals advance `sequence`, so they necessarily stale the stamp.
- `activate_generation_inner` is already on the 2B `WriterConnection` path, so the writer-owned stamp can be added in storage without reintroducing `ManagedPostgres`.
- Main chunk retrieval still treats `embedding_fingerprint` as a SQL filter in dense/hybrid modes; it does not compare it to `corpus_state.embedding_fingerprint` before retrieval.

1. **Q1: Read-path fork**

   Use `signature != "public"` as the data-topology discriminator: once `corpus_state` has an installed active corpus, the read path must be stamp-only. A client topology with no matching readiness stamp is not legitimate after 3A; it is a writer/apply fault and should error, including immediately after activation or incremental apply.

   The adjustment: do not use `signature == "public"` by itself as permission to compute and write in every context. An empty shared-server site also has signature `public`, but its read role is still supposed to be read-only and may not even have `SELECT` on the producer/public working tables. Make the API split explicit:

   - `load_query_readiness` / `load_query_readiness_with_client`: read-only lookup, missing or stale stamp errors for installed topology.
   - `load_or_compute_query_readiness` or `load_or_compute_local_query_readiness`: legacy local producer fallback, allowed to compute only for `public`.

   Then wire the local self-managed CLI through the legacy public fallback, and wire the shared-server/site read path through lookup-only. This preserves producer workflows without making read-only site behavior depend on implicit role permissions.

   Right after migration before first apply, there is no client corpus. For shared-server query serving, that should be "no active corpus / index unavailable", not a readiness recompute. For local producer/public workflows, compute-on-read can remain the compatibility path.

2. **Q2: Writer stamp inside activation**

   Sound, with one implementation constraint: the stamp helper should not use `apply_read_search_path`. Make coverage computation schema-qualified against the physical generation schema and callable with `&mut impl GenericClient`, including `postgres::Transaction`.

   The activation ordering should be:

   - write `generation_registry` and `corpus_state`;
   - write dense `index_manifest` rows;
   - grant read/view-owner visibility if applicable;
   - rebuild stable views;
   - stamp query readiness using the post-switch signature from the same transaction;
   - run the read-visibility probe;
   - commit.

   Because the stamp uses the writer transaction before `SET LOCAL ROLE <read>`, it avoids interference with the P2A probe. Stamping before the probe also means the final read-visible topology includes the readiness row in `public.index_manifest`.

   For 3A single-corpus, it is acceptable for the helper to target only the activated generation. Add an assertion or clear error if multiple active corpora are present; otherwise a single-corpus coverage report could be written under an aggregate multi-corpus signature, which would be wrong until 3C.

3. **Q3: Incremental restamp**

   Yes. Stamp inside `apply_incremental`'s existing cursor-gated transaction after the JSONL diff and postcondition checks, and after `advance_corpus_cursor`, because the new `sequence` is part of the signature. Returning an error before `tx.commit()` rolls back the row mutations and cursor advance.

   The coverage is available in-transaction: incrementals mutate the active physical generation tables directly, and the helper can compute against `schema_for_generation(active_generation)`.

   One trap: idempotent no-op branches can bypass stamping. Today `apply_incremental` returns `AlreadyApplied` before the ordinary postcondition/advance path; baseline/rebaseline also have an idempotency decision before activation. Decide whether no-op writer calls should verify/repair the stamp. At minimum, tests should include the normal successful incremental path; ideally no-op apply should either leave an existing matching stamp alone or restamp/verify and fail if the stamp is missing.

4. **Q4: Reuse `index_manifest['query_readiness']`**

   Reuse it for 3A. The existing JSON shape already stores `{ signature, report }`, and `index_manifest` is already granted to the read role. A new readiness table would add migration and grant churn without changing the invariant.

   Rename the mental model from "cache" to "stamp": comments, function names, and errors should stop saying a stale installed-topology value "forces a recompute." For installed topology, stale/missing/malformed means writer fault. For `public` legacy local mode, the same key can remain a cache.

   Keep the current aggregate signature strategy for now. It aligns with the architecture's multi-corpus direction, while 3A is single-corpus. Just avoid pretending a single physical generation coverage check proves aggregate readiness when more than one active corpus exists.

5. **Q5: Fingerprint preflight placement and error**

   Put the preflight in storage, at the `hybrid_candidates_json` / retrieval-entry boundary, before building SQL or setting probes. That is the single path used by search, compare, eval, session, and later the P4 query service. CLI-only placement would leave future handlers and some eval paths able to bypass the check.

   For 3A single-corpus behavior:

   - if `retrieval_mode` does not use dense, do nothing;
   - if dense/hybrid and there is exactly one active corpus, require `query.embedding_fingerprint == corpus_state.embedding_fingerprint`;
   - if dense/hybrid and there is no active corpus, preserve the legacy public path for now or explicitly document it as outside the site path;
   - if more than one active corpus, return "multi-corpus fingerprint preflight deferred to 3C" rather than silently choosing one.

   The storage error can be `StorageError::Retrieval { message }` carrying the machine token `embedding_fingerprint_mismatch` and both fingerprints. The CLI/site surface should map it to an `ErrorObject`, not a package-apply `RejectError`. The `RejectCode::EmbeddingFingerprintMismatch` vocabulary is still the right token to reuse in the message or structured future error, but ordinary query/session failures should remain `ErrorObject`s.

6. **Q6: Scope / 3A vs 3B**

   The 3A/3B boundary is safe if 3A stays narrow:

   - no snapshot store yet;
   - no broad read API rewrite;
   - no hot-search fan-out;
   - no replacing `execute_read_sql`.

   Add read-only lookup semantics and writer stamping now; let 3B put those reads inside a real snapshot later. Designing the readiness helper as `*_with_client` / `GenericClient`-based will make the later snapshot work easier without starting it in 3A.

7. **Q7: Invariants, negatives, and ordering**

   Add these tests or equivalent acceptance checks:

   - Installed topology + deleted `query_readiness` row + read role: read returns a clear readiness error and performs no write.
   - Installed topology + stale signature row after sequence advance: read errors, no recompute.
   - Activation with incomplete projection or embedding coverage: transaction rolls back, no active generation/cursor advance.
   - Incremental with incomplete coverage after the diff: transaction rolls back, cursor sequence unchanged, active generation data unchanged.
   - Successful incremental: restamps to the new `corpus:generation:sequence`, then a SELECT-only read succeeds without writing.
   - Hybrid and explicit dense with wrong query fingerprint: fail before retrieval; no lexical-only fallback and no empty dense result pretending success.
   - BM25 mode with mismatched or absent embedding config remains unaffected by the dense preflight.

   The existing lock ordering is fine. Baseline/rebaseline already serialize the long build under the per-corpus apply lock and then run a short activation transaction under the switch advisory lock. Incremental already uses a single cursor-gated transaction. The stamp belongs inside those existing critical sections; do not add an extra connection or post-commit repair step.

**Additional 3A risks**

- The current `load_readiness_metrics` takes `&mut postgres::Client`; the stamp path needs transaction-compatible generic helpers.
- Comments and names still describe `query_readiness` as a cache; if left unchanged, future code will reintroduce compute-on-read.
- `coverage_is_complete` requires `total > 0`. That is probably correct for legal corpora, but it means an empty package cannot activate. Keep that explicit in tests.
- Existing idempotent apply paths can bypass a new stamp hook unless deliberately handled.
- `zone_candidates_json` has required dense inputs but no active-generation fingerprint comparison. The user asked for main chunk search in 3A; still track zone parity as a follow-up or prove the dedicated zone readiness gate already covers it sufficiently.
