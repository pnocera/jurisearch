# P6 Trust & Gating Review (Round 2)

## Findings

No findings.

## Verification Notes

- The r1 BLOCKER is fixed in the actual trust path. `license_token_blobs` now selects only serialized token blobs by corpus, with `tier`, `license_epoch`, and `not_after` documented as denormalized index columns rather than trust gates (`crates/jurisearch-storage/src/trust.rs:109-131`). `check_entitlement` re-deserializes each `Signed<LicenseToken>`, re-verifies it against a license-purpose verifier, checks signed payload coverage, and evaluates the signed payload's own `not_after` with the DB clock before accepting entitlement (`crates/jurisearch-syncd/src/trust.rs:47-85`). I did not find a remaining path where the mutable local `license_token.not_after` column governs expiry.
- The expiry-tamper regression covers the bypass directly: it installs an expired signed token, clears only `jurisearch_control.license_token.not_after`, then still expects an entitlement rejection and an empty `corpus_status` (`crates/jurisearch-package-build/tests/trust_gating.rs:228-293`). That would fail under the old column-trust behavior.
- The redundant apply-side `canonical_digest(manifest)` step is gone. Baseline/re-baseline apply now verifies the embedded signature, runs compatibility and entitlement gates, then enforces payload and aggregate digests from bytes read off disk in `verify_per_file_digests` (`crates/jurisearch-syncd/src/apply.rs:119-155`, `crates/jurisearch-syncd/src/apply.rs:554-615`). Incremental apply follows the same verified-manifest-before-gates shape (`crates/jurisearch-syncd/src/apply.rs:808-830`).
- Lowercase-strict hex is enforced at both trust-anchor and signature verification sites. `decode_lower_hex_exact` rejects wrong length, uppercase, and non-hex input before decoding; `Ed25519Verifier::from_anchors` maps malformed 32-byte public keys to `MalformedKey`, and `verify_bytes` maps malformed 64-byte signatures to `Invalid` (`crates/jurisearch-package/src/crypto.rs:179-193`, `crates/jurisearch-package/src/crypto.rs:298-346`). The strict-decoder and malformed-anchor tests cover uppercase, short, and non-hex cases (`crates/jurisearch-package/src/crypto.rs:405-457`).
- The wrong-corpus entitlement test is now tied to the intended failure mode: it checks the error mentions entitlement and that `corpus_status` remains empty before a valid core token is installed (`crates/jurisearch-package-build/tests/trust_gating.rs:207-223`).

## Validation Run

- `cargo fmt --check`
- `cargo test -p jurisearch-package`
- `cargo test -p jurisearch-package-build --test trust_gating`
- `cargo test -p jurisearch-storage`
- `cargo test -p jurisearch-cli`
- `cargo test -p jurisearch-package-build`
- `cargo clippy --all-targets` completed with exit code 0; it still reports pre-existing warnings in unrelated `jurisearch-official-api`, `jurisearch-storage`, and `jurisearch-cli` code, with no new warning in the P6 trust-gating code.

VERDICT: GO
