# P9 Operated Producer Scope Validation

## Verdict

**Cut P9 to a local-operated publishing vertical slice, not a production hosting platform.** The minimal coherent P9 code should make this path work end-to-end:

producer DB mutation/enrichment -> package build -> publish to a deterministic filesystem serving root -> signed remote manifest -> client `update` with `DirectoryCatchupSource` -> existing P7/P6/P4/P5 apply gates.

Defer real TLS, CDN/object-store choice, cron/daemon cadence, key custody, quota policy, and measured performance calibration to ops/P10. Do **not** build a toy HTTP auth server just to satisfy the word “hosting”; it would not prove the real edge-auth property and would add a second transport path before the signed package contract is fully exercised.

The non-negotiable P9 code deliverables are:

1. Remote-manifest builder in `jurisearch-package-build`.
2. Filesystem publisher plus production `DirectoryCatchupSource`.
3. Producer package CLI and consumer update/subscribe/trust CLI.
4. A testable `producer_cycle` orchestration seam.
5. Proactive-enrichment orchestration using the existing `ingest enrich-zones`/zone-unit pipeline, not a schema rewrite.

## Source Constraints Verified

P7 already added the important remote-manifest contract fields: `BaselineRef.package_kind`, `minimum_client_version`, `uncompressed_size_bytes`, `estimated_load_seconds`, and the expanded `CatchupPolicy`. `run_catchup` already binds fetched artifacts to the signed remote digest and uses `apply_media_auto` for baseline vs re-baseline dispatch.

The current `CatchupSource` trait is in `jurisearch-syncd::planner`, but its only real implementation is test-local (`LocalSource` in `catchup_loop.rs`). P9 needs a production filesystem implementation.

`package_catalog` stores chain identity, package kind, baseline/generation, change-seq window, digest, manifest digest, schema/fingerprint/builders, status, and timestamps. It does **not** store artifact directory, artifact URI, compressed size, uncompressed size, row counts, or estimated apply/load seconds. A remote-manifest builder therefore cannot be “catalog only”; it must read the embedded manifests/artifact files from a staging or publish root, and use the catalog to validate chain/order/status.

`decision_zones` is still described as a lazy cache, but the code already has an eager producer command: `ingest enrich-zones`, plus `build-zone-units` and `embed-zone-units`. The write path emits outbox events for `decision_zones`, `zone_units`, and `zone_unit_embeddings`, so packages already ship enriched data when the producer has created it.

`jurisearch-syncd` currently has only `apply` and `status` subcommands. It loads a package verifier from trust anchors, but there is no CLI to install trust anchors, install license tokens, or run the P7 update loop.

## P9 Scope

Ship these as the P9 vertical slice:

- `jurisearch-package-build::remote_manifest`: build `Signed<RemoteManifest>` from `package_catalog` + a publish/staging artifact root.
- `jurisearch-package-build::publish`: copy or stage artifact directories under a deterministic root, write signed remote manifests atomically, and optionally mark catalog rows published.
- `jurisearch-syncd::DirectoryCatchupSource`: maps signed `artifact_uri` values or package IDs to local artifact directories under the published root.
- Producer CLI: `jurisearch package build ...`, `jurisearch package publish`, `jurisearch package list`, `jurisearch package verify`.
- Consumer CLI: `jurisearch-syncd trust install-anchor`, `jurisearch-syncd subscribe`, `jurisearch-syncd update`, richer `jurisearch-syncd status`.
- `producer_cycle(corpus, ...)`: a function that can be invoked by tests/CLI and later by cron/daemon.

Defer:

- HTTP server implementation
- TLS termination
- CDN/object-store provider
- edge credential implementation
- cron/systemd/Kubernetes scheduler
- KMS/HSM/key rotation automation
- measured apply-cost calibration
- artifact archive format if you decide to serve tarballs later

## Q1: Hosting Slice and Entitlement

**Filesystem publish + apply-precondition entitlement is sufficient for the code vertical slice.** It proves the data contract: signed remote manifest, signed embedded manifest, digest binding, local entitlement token enforcement, and gap-free update apply.

The minimum hosting code is:

- deterministic published layout, e.g. `root/<corpus>/manifest.json` and `root/<corpus>/packages/<package_id>/...`
- atomic writes: stage under `.tmp`, then rename
- `DirectoryCatchupSource` that only resolves artifacts under that root and rejects path traversal/unknown URI schemes
- a `verify-published-root`/`package verify` path that reads the signed remote manifest, verifies artifacts referenced by it exist, and checks each remote `sha256` equals the embedded manifest's `integrity.artifact_sha256`

Do **not** implement a loopback auth server for P9. Edge auth is deployment-specific and the design explicitly leaves CDN/object-store as an ops decision. The code should make the expected object-store ACL shape obvious by using per-corpus paths and entitlement metadata, but the real “client without subscription is refused at the edge” acceptance belongs to an ops smoke or P10 deployment harness.

One caveat: if you want a P9 test named “entitlement end-to-end,” make it assert P6 apply refusal through `run_catchup` using a subscription package and no token. Do not claim it proves edge refusal.

## Q2: Remote-Manifest Builder

**Yes, it belongs in `jurisearch-package-build`, but it must read artifacts as well as the catalog.**

Recommended API:

```rust
build_remote_manifest(
    producer: &ManagedPostgres,
    corpus: &str,
    published_or_staging_root: &Path,
    signer: &dyn Signer,
    params: RemoteManifestParams,
) -> Result<Signed<RemoteManifest>, BuildError>
```

`RemoteManifestParams` should include publisher/environment/generated_at, catchup policy, retention settings, default estimates, entitlement listing, and URI base.

Builder rules:

- Acquire the per-corpus package build lock, or otherwise serialize against package build/publish, so the manifest does not see a half-built chain.
- Query catalog rows for the corpus ordered by `package_sequence`.
- Read each referenced artifact's `Signed<EmbeddedManifest>` from the artifact root.
- Verify catalog `package_digest` equals embedded `integrity.artifact_sha256`; reject mismatch.
- Use the embedded manifest for compatibility fields, package kind, from/to sequence, operations row counts, and embedded signature.
- Populate `BaselineRef.signature` / `RemotePackageEntry.signature` with the embedded manifest's `Signed<EmbeddedManifest>.signature`. P7 does not enforce those yet, but they are meaningful and avoid placeholder signatures in published metadata.
- Sign the whole `RemoteManifest`.

Add storage read helpers rather than issuing raw SQL from the builder:

- `catalog_rows_for_corpus(corpus)`
- `latest_media_package_for_corpus(corpus)`
- `mark_package_published(package_id, published_at)` if you want `status='published'`

Do not rely on `latest_package_for_corpus` alone. It only returns the newest row; the remote manifest needs the retained chain and active media root.

## Q3: CLI Placement

**Use the existing binaries, but keep implementations modular.**

Producer commands should extend `jurisearch-cli` under a new top-level `package` command, matching the implementation plan. That binary already owns producer-side ingest, index opening, and local DB management. The clap tree is large, but the established pattern is clear: argument definitions in `args.rs`, dispatch in `dispatch.rs`, implementation in a module. Add a `package.rs` module and keep build/publish logic out of `main.rs`.

Consumer commands should extend `jurisearch-syncd`:

- `trust install-anchor --purpose package|license --key-id ... --key-epoch ... --public-key-hex ...`
- `subscribe --token-json <path>` using `install_verified_license_token`
- `update --corpus <corpus> --manifest <path> --source-root <path>` for P9 filesystem mode
- `status [--corpus] [--with-plan --manifest <path>]`

The trust-anchor CLI is a P9 gap in your proposal. `subscribe` verifies license tokens against license-purpose trust anchors, and package apply verifies manifests against package-purpose trust anchors. Without a way to install anchors, the production client cannot bootstrap trust except through tests or manual SQL.

Avoid overloading `jurisearch sync`. The plan is right that existing `sync` means local official-source archive-delta ingest, not server package update.

## Q4: Proactive Enrichment

**Defer the pipeline rewrite, not the orchestration.** The current code already has the core pieces:

- `ingest enrich-zones` eagerly backfills `decision_zones` for `cass`/`inca`
- `ingest build-zone-units` derives retrieval units
- `ingest embed-zone-units` builds embeddings/finalizes zone dense index
- all relevant tables are replicated and captured by outbox hooks

The minimal P9 code should make `producer_cycle` able to run those steps before package build when configured. It does not need to remove the lazy `fetch --part --online` cache code or redesign `decision_zones`.

The acceptance condition should be scoped as:

- when producer enrichment is enabled and credentials are configured, `producer_cycle` runs `enrich-zones` for `cass`/`inca`, derives/embeds zone units, builds a package, and the client can serve the packaged zones with no upstream call;
- out-of-coverage decisions remain `zone_accurate=false`.

If credentials are absent, the cycle should fail closed for an operated production profile or explicitly report `enrichment_skipped` for a local/test profile. Do not silently publish a package that claims proactive enrichment was run.

## Q5: Retention, `min_available_sequence`, and `catchup_ranges`

This is the subtle part. Compute from package-chain coordinates, not global `change_seq`.

Recommended model:

- `active_baseline` is the newest published media package for the corpus: `package_kind IN ('baseline','rebaseline')`.
- `head_sequence` is the newest published package sequence for the corpus, including incrementals after that media root.
- `packages` lists retained **incremental** packages only. Do not encode re-baselines as `RemotePackageEntry`; they are media roots represented by `active_baseline`.
- Retain only incrementals with `from_sequence >= active_baseline.sequence` and within the retention policy.
- `min_available_sequence` is the earliest `from_sequence` of the retained incremental chain. If there are no retained incrementals, set it to `active_baseline.sequence`.
- Add an open `RequiresBaseline` range below `min_available_sequence`.
- Add bounded `RequiresBaseline` ranges for known retained holes or superseded/reissue regions if you publish such history; otherwise the P7 planner will route gaps to baseline anyway.
- Ensure retained incrementals are gap-free from `min_available_sequence` to `head_sequence`; if not, either lower `head_sequence` to the coherent prefix or publish a re-baseline and make it active.

Concrete edge cases:

- If latest package is a re-baseline and no post-rebaseline incrementals exist, `head_sequence == active_baseline.sequence` and `packages` is empty.
- A first baseline at sequence 1 cannot catch up an installed client at sequence > 1; if retention drops required incrementals, the producer must publish a forward re-baseline. P7 already blocks when the active media root is not catch-up-capable.
- For retained incrementals, `RemotePackageEntry.from_sequence`/`to_sequence` should come from the embedded manifest, not inferred from catalog `package_sequence` alone.

## Q6: Existing Code Risks

The biggest risk is that `package_catalog` is not a publish catalog. It has status and timestamps, but no artifact location, URI, or size fields. For P9 either:

- make the publish root layout deterministic from `corpus` + `package_id`/`baseline_id`, and have the remote-manifest builder read artifacts from that root; or
- add a small producer-side publish metadata table.

Prefer deterministic layout for P9; add a table only if operations need multiple publish targets.

Other risks:

- `jurisearch-package-build` currently has no remote-manifest/publish module and no filesystem walk helpers. Add these there rather than in `syncd`.
- `jurisearch-cli` currently does not depend on `jurisearch-package` or `jurisearch-package-build`; producer package CLI will need those deps.
- `jurisearch-syncd` has no trust-anchor install CLI, so a client update command would be unusable in production until that is added.
- Per-entry/ref `signature` fields are currently placeholders in P7 tests. P9 should populate them from the embedded manifest signature, even if the whole signed remote manifest remains the authoritative listing signature.
- Artifact sizes are not stored. For directory artifacts, define `uncompressed_size_bytes` as the sum of `manifest.json` + payload file bytes and set `compressed_size_bytes = uncompressed_size_bytes` unless/until you introduce a compressed transport artifact. Do not fake compression ratios.
- Atomic publish matters: never overwrite `manifest.json` before every artifact it references is staged and verified.

## Minimal P9 Test Matrix

1. Build baseline + two incrementals, publish them, build signed remote manifest, run `syncd update` from a client at the baseline cursor through `DirectoryCatchupSource`, assert head convergence/digests.
2. Delete or omit a retained package from the publish root, run `package verify`, assert it fails before client update.
3. Remote manifest retention edge: client at `min_available_sequence - 1` routes to `FreshBaseline`; client at `min_available_sequence` routes to incrementals.
4. Subscription package with no token fails at apply through `run_catchup`; installing a signed token lets update proceed.
5. Producer cycle with enrichment disabled/enabled reports explicit enrichment status and publishes a manifest whose artifacts include `decision_zones` when enrichment was run.

## Bottom Line

Your proposed slice is directionally right, but cut P9 around a signed filesystem-published distribution loop. Add remote-manifest build/publish in `jurisearch-package-build`, promote `DirectoryCatchupSource` and update/subscribe/trust CLIs in `jurisearch-syncd`, keep producer package commands in `jurisearch-cli`, and make `producer_cycle` a callable orchestration seam. Defer real TLS/CDN/cron/key-ops and measured cost calibration. Do not defer enrichment orchestration entirely: run the existing eager enrichment pipeline before packaging, but defer the deeper lazy-cache rewrite.
