# P3 Baseline Vertical Slice Code Review r4

## Findings

### WARN 1 - The aggregate digest is still computed from unverified manifest map entries

The r3 fix now uses the shared `jurisearch_package::artifact::aggregate_payload_digest` helper on both sides: the producer derives `payload_digest` from that helper and stores it in both `integrity.artifact_sha256` / `uncompressed_payload_digest` plus the producer catalog package digest (`crates/jurisearch-package-build/src/baseline.rs:165`, `crates/jurisearch-package-build/src/baseline.rs:168`, `crates/jurisearch-package-build/src/baseline.rs:237`), and the applier compares the recomputed aggregate to both integrity digests before idempotency, load, or activation (`crates/jurisearch-syncd/src/apply.rs:85`, `crates/jurisearch-syncd/src/apply.rs:478`, `crates/jurisearch-syncd/src/apply.rs:482`, `crates/jurisearch-syncd/src/apply.rs:491`).

The remaining gap is that the consumer does not actually compute the aggregate over the verified per-file digest set. `verify_per_file_digests` reads and hashes each entry in `manifest.payload.files`, and checks that each such entry is present in `manifest.integrity.per_file_digests` (`crates/jurisearch-syncd/src/apply.rs:452`, `crates/jurisearch-syncd/src/apply.rs:466`). But it then passes the whole signed `manifest.integrity.per_file_digests` map into `aggregate_payload_digest` (`crates/jurisearch-syncd/src/apply.rs:478`). There is no equality/cardinality check proving every digest in that map came from a file just read from disk, and the aggregate helper includes any map entry whose table appears in `apply_order` (`crates/jurisearch-package/src/artifact.rs:59`) while `copy_payload_in` explicitly allows `apply_order` entries with no payload file (`crates/jurisearch-syncd/src/apply.rs:573`).

That means a resealed manifest can keep or add an `integrity.per_file_digests["some_apply_order_table.copybin"]` value for a file that is not listed in `payload.files`; the aggregate can be made to match both integrity digests, yet that digest was never proven against artifact bytes. For zero-row/omitted payloads this can still activate, stamping a cursor/package digest partly derived from bytes that were not applied. That is the same class of chain-link problem as r3 WARN-1: the cursor digest is not strictly derived from the verified applied payload bytes.

Concrete fix: inside `verify_per_file_digests`, build a fresh `BTreeMap` from the payload files actually read and hashed, reject duplicate payload file names, reject unless this verified map exactly equals `manifest.integrity.per_file_digests`, and call `aggregate_payload_digest(&verified_per_file_digests, &manifest.payload.apply_order)`. Add a negative regression that removes a payload file entry or inserts an extra apply-order digest into `integrity.per_file_digests`, recomputes both aggregate integrity fields with the shared helper, re-seals, and proves apply rejects before activation.

## Resolved r3 Checks

- WARN-1 is partially resolved on the normal path: there is one shared aggregate helper in `jurisearch-package`, producer and consumer both call it, and the applier compares the aggregate to both `artifact_sha256` and `uncompressed_payload_digest` before load/activation (`crates/jurisearch-package/src/artifact.rs:55`, `crates/jurisearch-package-build/src/baseline.rs:168`, `crates/jurisearch-syncd/src/apply.rs:478`, `crates/jurisearch-syncd/src/apply.rs:482`, `crates/jurisearch-syncd/src/apply.rs:491`). The new `apply_rejects_a_tampered_aggregate_artifact_digest` covers tampering `artifact_sha256` alone (`crates/jurisearch-package-build/tests/baseline_loopback.rs:336`).
- WARN-2 is resolved in the apply ordering: after generation index build and catalog validation, but before postconditions and `activate_generation`, `apply_baseline` calls `write_dense_index_manifests` (`crates/jurisearch-syncd/src/apply.rs:123`, `crates/jurisearch-syncd/src/apply.rs:128`, `crates/jurisearch-syncd/src/apply.rs:130`). The writer maps the signed dense IVFFlat entries to `embedding` / `zone_embedding` rows and passes the package-declared `lists` and `probes` to storage (`crates/jurisearch-syncd/src/apply.rs:343`, `crates/jurisearch-syncd/src/apply.rs:350`, `crates/jurisearch-syncd/src/apply.rs:357`).
- `upsert_generation_dense_manifest` persists the query-side `vector_index.lists` and `vector_index.default_probes` fields the retrieval path reads (`crates/jurisearch-storage/src/generations.rs:441`, `crates/jurisearch-storage/src/generations.rs:446`, `crates/jurisearch-storage/src/generations.rs:450`, `crates/jurisearch-storage/src/generations.rs:451`, `crates/jurisearch-storage/src/retrieval/sql.rs:21`).
- `validate_index_contract` now rejects a signed IVFFlat `probes` value that does not equal `recommended_probes(lists)` (`crates/jurisearch-syncd/src/apply.rs:324`, `crates/jurisearch-syncd/src/apply.rs:326`, `crates/jurisearch-syncd/src/apply.rs:327`), and the new tamper regression covers this path (`crates/jurisearch-package-build/tests/baseline_loopback.rs:353`).

## Validation

- `cargo fmt --check`
- `cargo test -p jurisearch-package aggregate_digest_is_order_sensitive_and_stable`

I did not rerun the full managed-PG workspace suite.

VERDICT: FIXES_REQUIRED
