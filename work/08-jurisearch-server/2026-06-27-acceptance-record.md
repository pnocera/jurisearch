# Central-ingest package-distribution — acceptance record (P10)

This is the repeatable acceptance evidence for the central-ingest packaged-distribution system
(plan phases P0–P10). It maps each design invariant (§13) to the test that **proves** it, records the
§15 implementation-measurement decisions as facts, and documents the operated-acceptance run whose
literal long-running / two-machine forms are ops evidence (the codebase ships the deterministic proofs).

All tests run against the project's managed-PostgreSQL harness
(`JURISEARCH_PG_CONFIG=/home/pierre/.pgrx/18.4/pgrx-install/bin/pg_config`); each skips cleanly when no
managed PG is available.

## Invariant → proving test (§13, the section-5 matrix)

| Invariant (§13) | Proven by |
|---|---|
| **INV-1** Three event kinds incl. in-place base-row updates | `jurisearch-package-build/tests/incremental_loopback.rs` — `valid_to`-close REPLICATES to the active generation (not just inserts). |
| **INV-2** Derived rebuilds = document-scoped `replace_set`; ordered, gap-free | `incremental_loopback.rs` — a `replace_set` drops a chunk leaving NO stale BM25-visible row; out-of-order apply → `sequence_gap`. |
| **INV-3** Atomic, no partial movement; idempotent via cursor | `incremental_loopback.rs` (re-apply no-op) + `concurrency_soak.rs` — apply fails CLEAN under advisory-lock contention with no cursor movement; a concurrent reader only ever sees old-or-new committed state. |
| **INV-4** Per-corpus generations behind stable views; re-baseline repoints only the affected corpus | `rebaseline_loopback.rs` — a `core` re-baseline leaves `inpi`'s generation/cursor/view untouched; a pinned `document_id` survives. |
| **INV-5** `jurisearch_app` + `jurisearch_control` outlive every generation | `rebaseline_loopback.rs` + `reference_validation.rs` — full-row `jurisearch_app` snapshot is byte-identical across an incremental AND a re-baseline. |
| **INV-6** Index materialisation is part of apply/activation, never after cursor advance | `jurisearch-storage/tests/generations.rs::a_loaded_generation_has_the_full_index_inventory_before_activation` (PK/BM25/IVFFlat inventory exists in the generation BEFORE activation), `::query_readiness_resolves_to_the_active_generation_and_a_public_cache_cannot_authorize_it` (readiness is measured from the active generation, never stale `public`), `::activation_validates_building_state_and_cursor` (a rejected switch leaves `corpus_state` unchanged). Package-level loopback + the signed index-build contract (tampered IVFFlat lists/probes rejected before the switch): `baseline_loopback.rs`. |
| **INV-7** Soft validated references; pin vs as-of | `reference_validation.rs` — a pin resolves across incremental + re-baseline; a logical (`version_group` + `as_of_date`) ref resolves the right version and is flagged `changed` on supersession. |
| **INV-8** Client builds indexes; prebuilt is a fenced physical variant only | `baseline_loopback.rs` — the client builds IVFFlat `lists` at the corpus rowcount + BM25, and enforces the signed index contract (tampered lists/probes rejected). |
| **INV-9** Signed, self-sufficient; integrity/version/entitlement are apply preconditions; warn-and-reject, no partial cursor movement | `trust_gating.rs` (Ed25519 sign/verify, tamper → `signature_invalid`, subscription → `missing_entitlement`, expired-token column-tamper still refused) + `conformance_reject_codes.rs` — every §6.3 `RejectCode` is produced by a real path. |

Planner / catch-up / distribution: `jurisearch-syncd/src/planner.rs` (the §9.4 decision matrix + the
manifest corpus guard), `catchup_loop.rs` (an offline client converges to head in order; past-retention
→ baseline), `publish_distribution.rs` (build → publish → signed remote manifest → `update` → converge;
`producer_cycle` extends the chain; a missing/ tampered published artifact fails verify/build).

## §6.3 reject-code coverage

`conformance_reject_codes.rs` drives each of the 11 closed-vocabulary codes once through a real
plan/apply/trust path and asserts the observed set equals `RejectCode::all()`:
`SignatureInvalid`, `DigestMismatch`, `ClientTooOld`, `SchemaAhead`, `ExtensionMissing`,
`MissingEntitlement`, `SequenceGap`, `WrongGeneration`, `EmbeddingFingerprintMismatch`,
`BuilderVersionMismatch`, `BaselineRequired`. Each refusal is asserted on the structured
`SyncError::Reject { code, .. }`, not on substrings.

## §15 implementation-measurement decisions (resolved)

- **§15.1 — view vs function.** Stable per-relation **views** (`jurisearch_server.*`, `UNION ALL` over
  active per-corpus generations) are the chosen indirection for read transparency; hot indexed reads use
  the **physical generation schema** directly (resolved from `corpus_state.active_generation`), so
  index scans never pay the view's union cost. `activate_generation` repoints the views in the same
  short switch transaction as the cursor advance (§7.4).
- **§15.2 — per-file payload encoding.** Baselines/re-baselines ship per-table **COPY-binary** payload
  files (PG-major-pinned, loopback-only); incrementals ship **JSONL** diffs (portable, upsert/delete/
  replace-set via `jsonb_populate_record`). Real whole-archive/tarball hashing + compression are deferred
  to transport (the artifact digest is the aggregate over verified per-file digests).
- **§15.3 — catch-up thresholds.** The §9.4 thresholds (compressed/uncompressed ratios, apply-seconds
  budget, chain-length cap) are **manifest-configured** (`catchup_policy`), tunable per corpus without a
  client upgrade. Final numeric calibration is deferred to measured operated runs (the producer declares
  `estimated_apply_seconds` / `estimated_load_seconds`; the client enforces only hard local gates).

## Acceptance run (operated — long/two-machine forms are ops evidence)

The full producer→consumer loop is reproducible from the deterministic tests above. The operated
acceptance form (ops evidence, not gating ordinary test runs):

1. **Producer.** `jurisearch-package build {baseline|incremental|rebaseline} …` → `publish` →
   `publish-manifest`; `jurisearch-package verify --public-key-hex …` gates the published root.
2. **Consumer (fresh second machine).** `jurisearch-syncd trust install-anchor …`, optional
   `subscribe --token-json …`, then `jurisearch-syncd update --corpus … --source-root …` reproduces the
   producer state from the media baseline + network incrementals; `status --json` reports the cursor +
   compat stamps.
3. **Re-baseline survival.** A `core` re-baseline with rows in `jurisearch_app` referencing `core`:
   `jurisearch_app` is byte-identical (`rebaseline_loopback.rs`/`reference_validation.rs`), `inpi` stays
   queryable, and the reference validator flags exactly the changed/vanished targets.
4. **Refusals.** Every malformed/unauthorised/out-of-order input is refused with the right §6.3 code
   (`conformance_reject_codes.rs`), leaving the cursor untouched.

The literal 24h soak and the two-physical-machine run are operated acceptance evidence on the minimum
viable test bed (see the prerequisites doc); the codebase ships `concurrency_soak.rs` as the
deterministic concurrency proof and the operator soak entrypoint.
