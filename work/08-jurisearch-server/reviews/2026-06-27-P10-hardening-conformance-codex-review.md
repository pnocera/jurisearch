# P10 hardening / conformance review

## Findings

### WARN: INV-6 is over-claimed against the cited acceptance test

`work/08-jurisearch-server/2026-06-27-acceptance-record.md:21` says `baseline_loopback.rs` proves that the corpus is not query-ready until indexes are built inside the `building` generation and that the index contract is validated before the switch. The package-level test does prove the applied baseline is readable, records the cursor, persists the signed dense index contract, and rejects tampered IVFFlat parameters before activation (`crates/jurisearch-package-build/tests/baseline_loopback.rs:115`, `:179`, `:216`). It does not itself prove the stronger readiness/ordering wording.

The stronger INV-6 evidence exists, but it is not named in the acceptance row: `crates/jurisearch-storage/tests/generations.rs::a_loaded_generation_has_the_full_index_inventory_before_activation` proves the generation has PK/BM25/IVFFlat inventory before activation, `::query_readiness_resolves_to_the_active_generation_and_a_public_cache_cannot_authorize_it` proves readiness is measured from the active generation rather than stale `public`, and `::activation_validates_building_state_and_cursor` proves a rejected switch leaves `corpus_state` unchanged. Since the P10 brief requires the INV→test mapping to be accurate against the actual tests, the acceptance record should cite these tests for INV-6 instead of attributing the whole proof to `baseline_loopback.rs`.

Concrete fix: amend the INV-6 row to cite the three `crates/jurisearch-storage/tests/generations.rs` tests above, keeping `baseline_loopback.rs` only for the package-level loopback and signed index-contract apply path.

## Notes

The P10 code slice otherwise matches the agreed scope:

- `CorpusStatus` now carries `embedding_fingerprint`, `builder_versions`, `last_package_digest`, and `applied_at`, derives `Serialize`, and `jurisearch-syncd status --json` writes JSON to stdout without touching query stdout discipline.
- `concurrency_soak.rs` uses separate connections, proves advisory-lock contention fails without cursor movement then retries successfully, and uses set membership plus final convergence rather than a timing window for the reader proof.
- `conformance_reject_codes.rs` drives all 11 `RejectCode::all()` variants through real apply/trust paths and asserts structured `SyncError::Reject { code, .. }`; the tamper helper reseals all valid-signature cases and deliberately does not reseal the `SignatureInvalid` case.
- The remaining INV rows in the acceptance record are backed by the named package-build/storage tests I checked.

VERDICT: FIXES_REQUIRED
