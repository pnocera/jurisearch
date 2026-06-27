## Findings

No findings.

The four r1 issues are resolved in the current source:

- `jurisearch-package verify` is now read-only and public-key based. The CLI accepts `--public-key-hex`, builds an `Ed25519Verifier`, does not open the producer DB, and calls `verify_published_root` directly (`crates/jurisearch-package-build/src/bin/jurisearch_package.rs:285`). `verify_published_root` reads the actual published `<root>/<corpus>/manifest.json`, verifies the signed remote manifest, rejects a corpus mismatch, and then checks every referenced artifact's existence, embedded signature equality, public signature verification, and embedded `artifact_sha256` equality (`crates/jurisearch-package-build/src/verify.rs:38`, `crates/jurisearch-package-build/src/verify.rs:94`).
- The remote-manifest builder now binds the published embedded manifest to the cataloged identity, not just the payload digest. `CatalogRow` includes `manifest_digest`, the catalog query returns it, and `verify_catalog_identity` recomputes `canonical_digest(&signed.payload)` before checking package id, package kind, baseline id, generation, and `to_sequence`; retained incrementals also must be `Incremental` and a `+1` link (`crates/jurisearch-storage/src/package_catalog.rs:181`, `crates/jurisearch-package-build/src/remote_manifest.rs:231`).
- `syncd update` verifies the remote manifest signature and then calls `check_manifest_corpus` before reading the cursor or planning/applying catch-up (`crates/jurisearch-syncd/src/main.rs:133`, `crates/jurisearch-syncd/src/main.rs:142`). The guard rejects a signed manifest whose embedded corpus differs from the requested corpus (`crates/jurisearch-syncd/src/planner.rs:98`).
- `publish_package` no longer removes a live package directory. Existing package ids are immutable: same embedded `artifact_sha256` returns as an idempotent no-op, different content errors, and the `.tmp` copy plus rename path only runs when the destination did not already exist (`crates/jurisearch-package-build/src/publish.rs:39`).

Regression coverage is present for the r2 fixes:

- `verify_published_root_checks_the_actual_manifest_and_fails_on_tamper` corrupts the published manifest clients poll and requires verification to fail (`crates/jurisearch-package-build/tests/publish_distribution.rs:380`).
- `build_remote_manifest_rejects_a_tampered_embedded_identity` changes only embedded manifest identity while leaving payload files untouched and requires the remote-manifest build to fail (`crates/jurisearch-package-build/tests/publish_distribution.rs:461`).
- `the_corpus_guard_rejects_a_manifest_for_a_different_corpus` covers the new corpus guard (`crates/jurisearch-syncd/src/planner.rs:856`).

Validation run:

- `cargo test -p jurisearch-syncd --lib` passed: 16 tests.
- `cargo test -p jurisearch-package-build` passed, including the 5 `publish_distribution` tests.

VERDICT: GO
