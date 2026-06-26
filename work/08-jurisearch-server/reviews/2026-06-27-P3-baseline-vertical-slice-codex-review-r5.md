# P3 Baseline Vertical Slice Code Review r5

Review scope: current uncommitted and untracked P3 working tree in `/home/pierre/Work/jurisearch`, with the r4 finding rechecked against the live source and a fresh pass over the baseline build/apply/catalog/generation path. I focused especially on whether the cursor/package digest is now derived only from payload bytes the consumer actually verifies and applies.

## Findings

No findings.

## r4 Verification

The r4 warning is resolved. `apply_baseline` still runs digest verification before idempotency, generation creation, load, index build, postcondition validation, or activation (`crates/jurisearch-syncd/src/apply.rs:79`, `crates/jurisearch-syncd/src/apply.rs:85`, `crates/jurisearch-syncd/src/apply.rs:93`). The cursor identity remains `manifest.integrity.artifact_sha256`, but that value is no longer trusted until after the payload verification step has recomputed and checked it (`crates/jurisearch-syncd/src/apply.rs:97`).

`verify_per_file_digests` now builds a fresh `BTreeMap` from the files named in `manifest.payload.files`, reading each payload file from disk and hashing the actual bytes (`crates/jurisearch-syncd/src/apply.rs:452`, `crates/jurisearch-syncd/src/apply.rs:456`, `crates/jurisearch-syncd/src/apply.rs:459`, `crates/jurisearch-syncd/src/apply.rs:460`). It rejects a digest mismatch against the payload file declaration and rejects duplicate payload-file names before the map can collapse two entries into one (`crates/jurisearch-syncd/src/apply.rs:461`, `crates/jurisearch-syncd/src/apply.rs:470`). It then requires this verified map to equal `manifest.integrity.per_file_digests` exactly, so missing, extra, or different integrity entries are refused before any load work (`crates/jurisearch-syncd/src/apply.rs:477`, `crates/jurisearch-syncd/src/apply.rs:480`).

The aggregate digest is now computed from that verified map, not from the signed integrity map, using the shared producer/consumer `aggregate_payload_digest` definition in apply order (`crates/jurisearch-syncd/src/apply.rs:487`, `crates/jurisearch-syncd/src/apply.rs:490`; `crates/jurisearch-package/src/artifact.rs:55`). The result must match both `integrity.artifact_sha256` and `integrity.uncompressed_payload_digest` before idempotency can stamp the cursor with the package digest (`crates/jurisearch-syncd/src/apply.rs:494`, `crates/jurisearch-syncd/src/apply.rs:503`).

That closes the specific r4 gap. `copy_payload_in` may still skip an apply-order table with no payload file, which is the documented zero-row/absent-payload case, but an absent file can no longer contribute a digest to the aggregate: exact equality with the verified set rejects any corresponding `integrity.per_file_digests` entry before load (`crates/jurisearch-syncd/src/apply.rs:480`, `crates/jurisearch-syncd/src/apply.rs:585`). I did not find another path that binds the cursor/package digest to bytes that were not actually read and verified by the applier.

The producer side uses the same aggregate helper over its `per_file_digests` and `APPLY_ORDER`, writes the result to both embedded integrity fields, and stores the same payload/package digest in the producer catalog (`crates/jurisearch-package-build/src/baseline.rs:165`, `crates/jurisearch-package-build/src/baseline.rs:168`, `crates/jurisearch-package-build/src/baseline.rs:237`, `crates/jurisearch-package-build/src/baseline.rs:238`). The new regression `apply_rejects_a_payload_file_set_that_disagrees_with_integrity_digests` directly covers the r4 failure shape by removing a payload-file entry while leaving integrity to claim it, and asserts rejection before activation (`crates/jurisearch-package-build/tests/baseline_loopback.rs:370`).

## Additional Checks

I rechecked the adjacent r3/r4 surfaces that could have regressed while fixing this: same-snapshot baseline build, producer catalog package-vs-manifest digest separation, idempotency by package id plus package digest, index-contract validation for IVFFlat `lists` and `probes`, dense `index_manifest` persistence before activation, and generation activation after postcondition validation. I did not find a new issue in those paths.

## Validation

- `cargo fmt --check`

I did not rerun the full managed-PG suite in this pass; the brief already reports it green, and the r5 delta was source-reviewed against the targeted regression.

VERDICT: GO
