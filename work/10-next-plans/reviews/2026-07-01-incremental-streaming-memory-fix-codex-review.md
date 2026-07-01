## Findings

### WARN: The DB loopback "golden" test is not actually a golden for byte/order regressions

`crates/jurisearch-package-build/tests/incremental_loopback.rs:478-505`

The new loopback test validates internal consistency of the streamed artifact and determinism across two streamed builds, but it does not pin the bytes to pre-refactor expected values. It recomputes each file digest from the bytes just produced, recomputes the aggregate digest from those recomputed values, only checks that `canonical_digest(&manifest)` has a prefix, and then compares a second build produced by the same implementation. A deterministic regression in serialization, file push order, or `apply.operations` order could still pass as long as both builds agree.

This is especially important because array order in `payload.files` and `apply.operations` is part of the signed canonical manifest. The test currently uses `find`/maps for the main assertions and never asserts the exact ordered operation list, exact ordered payload file list, literal per-file digest constants, or a literal manifest digest constant. It therefore would not catch, for example, moving `documents.delete` before `documents.upsert` in `apply.operations` if the payload bytes and digest map remained deterministic.

Concrete fix: after capturing one known-good run in an environment with the required PG/extension stack, assert:

- exact ordered `manifest.apply.operations` entries, including table, op, and count;
- exact ordered `manifest.payload.files` entries, including file name, table, op, row count, and digest;
- exact `manifest.integrity.per_file_digests` constants for the expected files;
- exact `canonical_digest(&manifest)` constant.

Pinning either literal file bytes or per-file digest constants is enough for byte identity; pinning the ordered manifest fields and canonical manifest digest is what covers the signed-order contract.

### NIT: `HashingWriter` tests do not exercise partial-write or write-error behavior

`crates/jurisearch-package/src/canonical.rs:138-149`, `crates/jurisearch-package/src/canonical.rs:262-287`

The implementation is correct by inspection: it calls the inner writer first and updates the hasher only over `buf[..n]` from a successful `write`. However, the new unit tests only use `Vec<u8>`, whose writes succeed fully, so they do not prove the documented partial-write/failure property.

Concrete fix: add a tiny fake `Write` implementation that returns short `Ok(n)` writes and another that errors after accepting a prefix, then assert that `HashingWriter::finalize()` matches `digest_bytes` over exactly the accepted bytes and that failed writes do not advance the digest.

## Source Audit

I did not find a production correctness issue in the streaming rewrite.

The streaming path preserves the old byte layout. Base-table rows, graph edges, and replace-set envelopes are still serialized as compact JSON and followed by exactly one `\n` per row. `serde_json::to_writer` is byte-equivalent to the previous `serde_json::to_string` compact serializer for these row types, so the old `l1\n...lN\n` layout is preserved without leading/trailing extra bytes.

The load-bearing ordering also matches the old code. Base tables still iterate the `BTreeMap` order and finish upserts before deletes for each table. `graph_edges` still follows the base-table files. Replace sets still emit the fixed group order `[ChunksWithEmbeddings, ChunkEmbeddings, ZoneUnits, DecisionZones]`, with each group's `BTreeSet` document order preserved and the deleted-document skip retained only for `ChunksWithEmbeddings`.

Empty-file semantics are preserved. `JsonlOpWriter` opens lazily, so an op with no rows emits no file, no `PayloadFile`, and no `OperationCount`. Replace-set counts remain envelope counts, including an envelope whose nested `rows` are empty.

The memory fix removes the full-delta row accumulators from the rewritten payload sections. The remaining O(delta) term is the pre-existing `scopes_changed_for_corpus_with_client` result plus the coalescing `BTreeSet`s of unique scope keys. That is the disclosed strings-only term, and I did not see another full payload accumulator surviving in these sections.

The write safety model is consistent with the old artifact staging behavior. `HashingWriter` hashes only successfully accepted bytes, `JsonlOpWriter::finish` flushes before finalizing the digest, and a build error leaves only a partial staged artifact without a signed manifest/catalog row.

## Validation

I reviewed the working-tree diff and the current source for the touched paths. I did not rerun the test suite because this was requested as a review-only pass and the DB-backed loopback is known to require unavailable local services.

VERDICT: FIXES_REQUIRED
