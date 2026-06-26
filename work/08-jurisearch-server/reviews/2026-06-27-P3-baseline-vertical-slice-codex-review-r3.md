# P3 Baseline Vertical Slice Code Review r3

## Findings

### WARN 1 - The cursor package digest is still not derived from the applied artifact bytes

The r2 normal-path equality is fixed for artifacts produced by `build_baseline`: the producer computes `payload_digest`, stores it in `manifest.integrity.artifact_sha256`, and writes the same value to `package_catalog.package_digest` (`crates/jurisearch-package-build/src/baseline.rs:165`, `crates/jurisearch-package-build/src/baseline.rs:244`, `crates/jurisearch-package-build/src/baseline.rs:306`). The consumer also now uses `manifest.integrity.artifact_sha256` for the idempotency/cursor identity and stamps it into `corpus_state.last_package_digest` / `generation_registry.validation_digest` (`crates/jurisearch-syncd/src/apply.rs:94`, `crates/jurisearch-syncd/src/apply.rs:97`, `crates/jurisearch-syncd/src/apply.rs:134`).

The remaining gap is that apply never proves that this package digest actually matches the payload being applied. `verify_per_file_digests` reads every declared payload file and compares each file to its own declared digest, but it never recomputes the aggregate payload/package digest and never compares either `integrity.artifact_sha256` or `integrity.uncompressed_payload_digest` to the verified files (`crates/jurisearch-syncd/src/apply.rs:82`, `crates/jurisearch-syncd/src/apply.rs:393`). Because `apply_baseline` then trusts `artifact_sha256` directly as the cursor identity, an accepted manifest with `artifact_sha256` changed but all per-file digests unchanged would load the exact same bytes, pass postconditions, activate, and stamp a cursor digest that no longer equals the real package payload digest. That recreates the P4 chain-link mismatch that r2 was intended to remove, just through an internally inconsistent accepted manifest rather than the original manifest-vs-artifact confusion.

Concrete fix: after verifying per-file digests, recompute the same package digest the producer uses over the verified payload digest set in manifest apply order, and reject unless it equals both `manifest.integrity.artifact_sha256` and `manifest.integrity.uncompressed_payload_digest`. Add a negative regression next to the IVFFlat tamper test: change only `integrity.artifact_sha256` in the signed manifest, re-seal with the stub signer, and assert apply rejects before activation.

### WARN 2 - The baseline path still drops the signed IVFFlat `probes`/index-manifest contract on the floor

The manifest contract declares both IVFFlat `lists` and `probes`; the producer fills `probes` with `recommended_probes(lists)` (`crates/jurisearch-package/src/manifest/embedded.rs:177`, `crates/jurisearch-package-build/src/baseline.rs:335`). The r2 lists check is now real: `build_generation_indexes` creates each IVFFlat with the computed `lists`, and `validate_index_contract` checks method, replicated table, `embedding` column, and `lists` before activation (`crates/jurisearch-storage/src/generations.rs:362`, `crates/jurisearch-syncd/src/apply.rs:271`, `crates/jurisearch-syncd/src/apply.rs:308`).

But the `probes` half of that signed contract is neither validated nor persisted. `validate_index_contract` never reads `ivf.probes`, and the baseline index build does not write the `index_manifest` rows that the existing dense query path uses to pick the built-time `default_probes` (`crates/jurisearch-storage/src/retrieval/sql.rs:16`). The older finalize paths do persist those rows for `embedding` and `zone_embedding` (`crates/jurisearch-storage/src/dense.rs:236`, `crates/jurisearch-storage/src/zone_units.rs:614`), but the P3 generation build only returns index names/lists. On a fresh client baseline, dense retrieval therefore falls back to the hard-coded probe default rather than the package-declared value, and the cursor can advance before the "index manifests written" part of the baseline materialization contract is true.

Concrete fix: have the baseline index materializer upsert the `index_manifest` entries for both dense indexes from the actual built index shape: index name, method/opclass, `lists`, `default_probes` equal to the signed `ivf.probes`, and coverage counts. Then make `validate_index_contract` also compare the manifest-declared `probes` to the stored default or to `recommended_probes(lists)`. Add loopback assertions that the applied client has `index_manifest` rows whose `vector_index.lists` and `vector_index.default_probes` match the signed manifest, plus a tamper regression for mismatched `probes`.

## Resolved r2 Checks

- The r2 digest identity bug is resolved on the happy path: the producer catalog stores the payload/package digest, the consumer cursor stamps `manifest.integrity.artifact_sha256`, and the loopback test compares `corpus_state.last_package_digest` against `package_catalog.package_digest` (`crates/jurisearch-package-build/tests/baseline_loopback.rs:165`).
- `idempotency_decision` now compares both `last_package_id` and `last_package_digest`, so same-sequence but different-package replays are rejected instead of silently skipped (`crates/jurisearch-syncd/src/apply.rs:436`, `crates/jurisearch-syncd/src/apply.rs:449`).
- The IVFFlat lists contract is now checked against the real generation index through `pg_catalog` before `activate_generation`, including access method, target replicated table, indexed column, and `lists` reloption (`crates/jurisearch-syncd/src/apply.rs:119`, `crates/jurisearch-syncd/src/apply.rs:271`, `crates/jurisearch-syncd/src/apply.rs:331`, `crates/jurisearch-syncd/src/apply.rs:141`).
- The new negative regression tampers the signed IVFFlat `lists`, re-seals, and proves apply rejects without activating a corpus (`crates/jurisearch-package-build/tests/baseline_loopback.rs:192`).

## Validation

- `cargo fmt --check`
- `cargo test -p jurisearch-package-build --test baseline_loopback`
- `cargo test -p jurisearch-syncd`

I did not rerun the full managed-PG workspace suite.

VERDICT: FIXES_REQUIRED
