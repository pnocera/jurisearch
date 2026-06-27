# P6 Trust & Gating Review

## Findings

### BLOCKER: Expiry is enforced from mutable license-token metadata, not the signed token payload

`LicenseToken` makes `not_after` part of the signed entitlement claim (`crates/jurisearch-package/src/license.rs:24-33`), and P6 requires the installed `Signed<LicenseToken>` to be re-verified on every use before it is trusted. The current gate verifies the token signature and then checks only corpus/tier/epoch on the signed payload (`crates/jurisearch-syncd/src/trust.rs:62-70`). Expiry was already filtered earlier from the denormalized `jurisearch_control.license_token.not_after` column (`crates/jurisearch-storage/src/trust.rs:123-128`).

That leaves a bypass: take a once-valid signed token whose payload has an expired `not_after`, update only the local row's `not_after` column to `NULL` or a future timestamp, and the query will return the token. The signature still verifies because `token_json` was not changed, `covers()` ignores payload expiry, and the package applies. This is exactly the mutable-local-state problem the re-verification rule was meant to avoid.

Fix: after `signed.verify(&license_verifier)` succeeds, evaluate `signed.payload.not_after` with the DB clock before accepting coverage. For example, use the denormalized column only as an optional index, then run a payload-derived check such as `SELECT $1::timestamptz > now()` for `Some(not_after)` and skip the token on false/parse failure; or derive the SQL filter from `token_json->'payload'->>'not_after'` rather than the mutable column. Add a regression that installs an expired signed token, tampers only `license_token.not_after` to a future value/NULL, and still gets `MissingEntitlement`.

### WARN: The media apply path still has the redundant "internal canonical digest" step P6 was supposed to remove

`apply_media_package` verifies the signed embedded manifest first (`crates/jurisearch-syncd/src/apply.rs:120-125`), then verifies payload digests (`crates/jurisearch-syncd/src/apply.rs:152-153`), and then still calls `canonical_digest(manifest)` as an "integrity precondition" (`crates/jurisearch-syncd/src/apply.rs:155-157`). There is still no external expected manifest digest being compared here, and signature verification has already canonicalized the same payload. So this step is not an integrity check; it is a redundant canonicalization pass mapped to `DigestMismatch`.

Fix: remove the `canonical_digest(manifest)` block from the apply pipeline, or move canonicalization diagnostics into `Signed::verify` if a distinct error message is needed. Keep `artifact_sha256` enforcement where it is now: inside `verify_per_file_digests` after the embedded signature has been verified (`crates/jurisearch-syncd/src/apply.rs:557-617`).

### WARN: Hex decoding is length-checked but not canonical/lowercase-strict

The concrete Ed25519 path emits lowercase hex, but the verifier uses `hex::decode` directly for trust-anchor keys and signatures (`crates/jurisearch-package/src/crypto.rs:288`, `crates/jurisearch-package/src/crypto.rs:321`). `hex::decode` accepts uppercase hex. That means the implementation enforces valid hex and 32/64-byte length, but it does not enforce the documented lowercase wire form for `public_key_hex` / `signature_hex`.

This does not let an attacker forge a signature, but it weakens the strict wire contract P6 asked for and leaves multiple accepted encodings for the same bytes.

Fix: add a small helper like `decode_lower_hex_exact(value, expected_bytes)` that rejects non-ASCII-hex, uppercase A-F, odd length, and wrong byte length before decoding. Use it for both 32-byte public keys and 64-byte signatures, and add tests for uppercase key/signature, short key, and short signature.

### NIT: The wrong-corpus entitlement test is broader than the behavior it intends to prove

The test installs a verified wrong-corpus token, then only asserts `apply_baseline(...).is_err()` (`crates/jurisearch-package-build/tests/trust_gating.rs:207-213`). Source inspection shows the failure is genuinely from the entitlement gate, because `check_entitlement` runs before payload digest or mutation and re-checks signed corpus/tier coverage. But the assertion itself would also pass for a later digest/schema/cursor failure.

Fix: assert that the error string/code is `missing_entitlement` and that `corpus_status(&client)?` is still empty after the wrong-corpus attempt. The tamper test is better grounded: it mutates signed manifest payload without resealing and the apply path verifies the signature before any digest check.

## Confirmed P6 Scope

- Concrete Ed25519 landed in `jurisearch-package`: `Ed25519Signer::from_seed`, `(KeyId, KeyEpoch)` keyed `Ed25519Verifier::from_anchors`, `verify_strict`, and explicit `UnsupportedAlgorithm` / `UnknownKey` / `MalformedKey` / `Invalid` mapping are present.
- Builders now stamp `integrity.signature_algorithm` from `signer.algorithm()` in both baseline/re-baseline and incremental builders (`crates/jurisearch-package-build/src/baseline.rs:420-428`, `crates/jurisearch-package-build/src/incremental.rs:458-465`).
- Migration v23 creates `trust_anchor` with PK `(key_id, key_epoch, purpose)` and `license_token` with PK `(corpus, tier, license_epoch, audience)` (`crates/jurisearch-storage/src/migrations.rs:1050-1069`). `LicenseToken` lives in the contract crate.
- The apply paths verify `Signed<EmbeddedManifest>` before trusting manifest fields. `artifact_sha256` is only checked later from verified payload bytes in `verify_per_file_digests`; I did not find a pre-signature trust of that field.
- Version/schema/extension and entitlement gates run before row mutation. Incremental postconditions are checked in-transaction before `advance_corpus_cursor`.
- Production `jurisearch-syncd apply` builds its verifier through `load_package_verifier`; `AcceptAllVerifier` remains in loopback/tests, not the binary (`crates/jurisearch-syncd/src/main.rs:47-51`).
- The deliberate deferrals are reasonable for P6: remote manifest consumption/network planner is still P7; `RemotePackageEntry.signature` / `BaselineRef.signature` enforcement can wait while both remote and embedded manifests use `Signed<T>`; whole tarball hashing can wait for transport; and audience matching is not enforced in P6.

VERDICT: FIXES_REQUIRED
