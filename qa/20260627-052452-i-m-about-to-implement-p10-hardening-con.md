**Verdict: GO, with two scope adjustments.** P10 should not become another feature phase. The existing source already has the core contracts wired: `apply_incremental` runs in one transaction, takes `APPLY_ADVISORY_LOCK_KEY`, checks the cursor `FOR UPDATE`, validates postconditions before `advance_corpus_cursor`, and returns typed reject codes; `activate_generation_with_guard` does the same short-switch discipline for baseline/re-baseline activation. The right final deliverable is a thin conformance and operability layer over those paths, not a rewrite.

The adjustments are: make the soak harness short by default but parameterizable for a longer operated run, and close the status/observability gap by reusing the full cursor shape already present in `planner.rs` instead of maintaining two cursor models.

## 1. P10 Testable Slice

**GO** on your slice:

- deterministic concurrency/atomicity test;
- reject-code conformance over the closed `RejectCode::all()` vocabulary;
- `corpus status` enrichment;
- acceptance-gate Markdown recording which tests prove INV-1 through INV-9 and which §15 items are implementation decisions.

**ADJUST:** add the deterministic soak as a normal fast test plus an optional longer mode, not as a literal 24-hour CI test. The design's §12 requires the concurrency property; the 24h run is acceptance evidence on the operated test bed. Ship one harness that defaults to a small iteration count and can be run longer via env var, for example `JURISEARCH_SOAK_ITERS=10000 cargo test -p jurisearch-package-build package_distribution_concurrency_soak -- --ignored` if you want the long form ignored.

That gives P10 code that can prove the property during development and also serve as the operator's soak entrypoint without making CI timing part of correctness.

## 2. Concurrency Test Shape

Use separate Postgres connections against the same durable `ManagedPostgres`; do not share a `postgres::Client` across threads. Sharing the same managed cluster is fine, but each reader/apply path should open its own connection from the connection string or through `ManagedPostgres::client()`.

I would split the concurrency proof into two assertions in one test file:

1. **Fail-clean lock contention, deterministic.** Open a transaction on a second connection and take `pg_advisory_xact_lock(APPLY_ADVISORY_LOCK_KEY)`. Then call `apply_incremental` or the re-baseline activation path from another connection. Because the real code uses `pg_try_advisory_xact_lock`, it should return immediately with the existing refusal path (`WrongGeneration` in `apply_incremental`, `StorageError::Generations` for activation), and the cursor must remain unchanged. This is the robust non-flaky proof of "does not stall behind contention."

2. **Old-or-new reader visibility, bounded smoke.** Seed baseline, spawn a reader thread that repeatedly runs the real read path (`execute_read_sql`, so it goes through the same active-generation/search-path behavior as the CLI), then apply an incremental and a re-baseline. Every observed result must be in an allowed set: old value or new value, never empty, mixed, or partially advanced. After apply completes, assert the final value and cursor. Do not require the loop to observe both old and new during the race; that makes the test scheduling-dependent. The invariant is set membership during the race plus final convergence after commit.

For the re-baseline portion, prefer a small artifact but run enough read iterations around the switch to exercise `CREATE OR REPLACE VIEW` visibility. The core switch is already short by design, so the test should not depend on catching a precise microsecond window.

## 3. Status / Observability

**ADJUST, but small.** `corpus_status` currently reads only:

- `corpus`
- `active_generation`
- `sequence`
- `baseline_id`
- `schema_version`
- `last_package_id`

The richer cursor already exists as `ClientCursor` in `planner.rs` and reads `embedding_fingerprint`, `builder_versions`, and `last_package_digest`. Do not keep these divergent. Either extend `CorpusStatus` to match the cursor stamps or have `corpus_status` internally reuse a shared cursor-row mapper.

Add these fields at minimum:

- `embedding_fingerprint`
- `builder_versions`
- `last_package_digest`
- optionally `applied_at`, since `corpus_state` carries it and it is useful in operations.

Also add `jurisearch-syncd status --json`. Keep the existing human output for convenience, but P10's "structured stderr/JSON" requirement needs a stable machine-readable management output. The status command can write JSON to stdout because it is the management CLI's primary result; diagnostics and logs should still go to stderr. This does not affect query stdout discipline.

## 4. Reject-Code Conformance

**GO, but make it a conformance layer, not a giant duplicate loopback.** The contract crate already has `RejectCode::all()` and a vocabulary test, but P10 should prove each code is produced by at least one real plan/apply/trust path.

I recommend one consolidated conformance test file that collects observed codes in a `BTreeSet<RejectCode>` and asserts equality with `RejectCode::all()`. Keep the individual phase tests as the behavioral detail; the P10 test only needs to drive each refusal once.

Cheap real-path triggers:

- `SignatureInvalid`: tamper a signed embedded manifest or use an untrusted signing key.
- `DigestMismatch`: tamper a payload file, aggregate digest, previous package digest, or postcondition.
- `ClientTooOld`: raise `minimum_client_version`.
- `SchemaAhead`: raise manifest/schema version beyond `CURRENT_SCHEMA_VERSION`.
- `ExtensionMissing`: add an impossible required extension in the signed manifest.
- `MissingEntitlement`: subscription-tier package without a covering token.
- `SequenceGap`: apply the second incremental while the cursor is still at the baseline.
- `WrongGeneration`: hold the advisory lock, or tamper active-generation/baseline preconditions.
- `EmbeddingFingerprintMismatch`: tamper incremental precondition fingerprint after baseline apply.
- `BuilderVersionMismatch`: same for `builder_versions`.
- `BaselineRequired`: apply an incremental to a fresh client, or plan a client past retention where the active baseline is not a forward re-baseline.

If a few of these are already well-covered in phase tests, the conformance test can use small helper fixtures and should assert the returned `SyncError::Reject { code, .. }`, not just string contents. That is the main hardening gap in the current tests I saw: several tests still assert substrings.

## 5. Acceptance Gate / §15 Record

**GO.** Add a Markdown acceptance record under `work/08-jurisearch-server/` that maps:

- INV-1: `incremental_loopback.rs` valid_to close;
- INV-2: stale chunk / BM25-visible row drop and replace-set coverage;
- INV-3: re-apply no-op plus new P10 concurrency test;
- INV-4/5: `rebaseline_loopback.rs` inpi untouched and `jurisearch_app` survival;
- INV-6/8: `baseline_loopback.rs` index built before activation and client-build path;
- INV-7: `reference_validation.rs`;
- INV-9: `trust_gating.rs` plus P10 reject-code conformance;
- planner/catch-up: `planner.rs`, `catchup_loop.rs`, `publish_distribution.rs`.

For §15, record the implementation decisions as facts, not open questions:

- stable views are the chosen indirection, with physical-generation reads where index scans require it;
- baseline payloads are COPY-binary, incrementals are JSONL;
- catch-up thresholds are manifest-configured and final calibration is deferred to measured operated runs.

The literal two-machine and 24h soak are acceptance/ops evidence. The codebase should ship the repeatable commands and optional long harness, but not block ordinary test runs on those durations.

## 6. Remaining Code Gaps

I see three genuine P10 code gaps:

1. `corpus_status` lacks the full cursor stamps and the CLI lacks `--json`.
2. There is no deterministic advisory-lock contention test around the real apply/switch lock.
3. There is no single conformance assertion that every `RejectCode::all()` variant is produced by a real path.

Everything else you listed is already enforced by earlier code paths and should be documented in the acceptance record rather than rebuilt. Do not add new package formats, hosting, TLS, calibration, or a long-running daemon in P10.
