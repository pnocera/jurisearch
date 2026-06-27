# P10 hardening / conformance review (round 2)

## Findings

No BLOCKER/WARN/NIT findings.

## Notes

- The r1 WARN is addressed. `work/08-jurisearch-server/2026-06-27-acceptance-record.md:21` now maps INV-6 to the three storage tests that actually prove the stronger readiness/ordering claims: `activation_validates_building_state_and_cursor` (`crates/jurisearch-storage/tests/generations.rs:416`), `query_readiness_resolves_to_the_active_generation_and_a_public_cache_cannot_authorize_it` (`crates/jurisearch-storage/tests/generations.rs:527`), and `a_loaded_generation_has_the_full_index_inventory_before_activation` (`crates/jurisearch-storage/tests/generations.rs:717`).
- The INV-6 wording now keeps `baseline_loopback.rs` scoped to the package-level loopback and signed index-build contract. That matches the current source: the loopback proves baseline apply/read/cursor/digest behavior (`baseline_loopback.rs:115`, `:132`, `:146`, `:158`) and the signed IVFFlat contract rejection path before activation (`baseline_loopback.rs:216`, `:268`, `:274`, plus the probes tamper helper/test in the same file).
- The P10 status observability slice remains accurate. `CorpusStatus` derives `Serialize` and includes `embedding_fingerprint`, `builder_versions`, `last_package_digest`, and `applied_at` (`crates/jurisearch-syncd/src/status.rs:11`, `:18`, `:19`, `:21`, `:22`); `jurisearch-syncd status --json` serializes the status list as JSON to stdout (`crates/jurisearch-syncd/src/main.rs:175`).
- The concurrency soak evidence still matches INV-3: advisory-lock contention is held on a separate connection and the failed apply leaves the cursor unchanged before a retry succeeds (`crates/jurisearch-package-build/tests/concurrency_soak.rs:171`, `:181`, `:187`, `:193`); the reader test allows only committed old/new values during incremental and re-baseline apply and asserts final convergence (`concurrency_soak.rs:201`, `:234`, `:322`).
- The reject-code conformance evidence remains accurate. `conformance_reject_codes.rs` extracts only structured `SyncError::Reject { code, .. }` (`crates/jurisearch-package-build/tests/conformance_reject_codes.rs:155`), drives all eleven closed-vocabulary paths (`conformance_reject_codes.rs:235`, `:241`, `:244`, `:250`, `:256`, `:267`, `:273`, `:282`, `:285`, `:298`), and asserts the observed set equals `RejectCode::all()` (`conformance_reject_codes.rs:312`).
- The other invariant rows in the acceptance record still map to current tests: incremental valid-to/replace-set/sequence-gap/no-op/cursor refusal evidence is in `incremental_loopback.rs`; re-baseline isolation and app-data preservation are in `rebaseline_loopback.rs`; reference pin/as-of/change behavior is in `reference_validation.rs`; trust and entitlement gates are in `trust_gating.rs`; planner/catch-up/publish distribution claims are present in `planner.rs`, `catchup_loop.rs`, and `publish_distribution.rs`.

VERDICT: GO
