# P7 Planner + Size-Driven Catch-Up Design Validation

## Verdict

**ADJUST, then build.** The planner belongs in `jurisearch-syncd`, the size/cost policy should be manifest-driven, and the `CatchupSource` seam is the right way to keep P9 transport out of P7. The proposed algorithm is close to §9.4, but there are four correctness gaps to close before coding:

- `BaselineRef` needs enough metadata to apply the active baseline correctly. Today it does **not** say whether the artifact is `baseline` or `rebaseline`, but the public apply API requires choosing `apply_baseline` vs `apply_rebaseline`.
- Existing `corpus_status` does **not** expose `embedding_fingerprint`, `builder_versions`, or `last_package_digest`, even though the proposed planner needs at least fingerprint/builders/schema to detect reissue crossings. Add a planner-specific cursor query or extend `CorpusStatus`.
- A baseline fallback for an already-installed client must be forward-moving. If `active_baseline.sequence <= cursor.sequence`, it is not a valid catch-up baseline for that client.
- The catch-up loop must compare the fetched artifact digest to the signed remote entry/baseline `sha256` before applying. The existing apply functions verify the embedded manifest and payload bytes, but they do not know the remote manifest entry.

## Source Constraints Verified

`RemoteManifest` currently has the right high-level shape: `head_sequence`, `min_available_sequence`, `active_baseline`, `packages`, `catchup_ranges`, `catchup_policy`, `entitlement`, and `signing`. `RemotePackageEntry` already carries compressed/uncompressed sizes, estimated apply seconds, compatibility stamps, digest, and signature. `BaselineRef` currently carries only `compressed_size_bytes`, not uncompressed bytes or estimated load seconds.

`CatchupPolicy` currently has only `max_incremental_packages` and `max_cumulative_diff_to_baseline_permille`. The uncompressed-ratio and apply-time rules in design §9.4 are not representable yet.

P6 is present in the source. `jurisearch-syncd::trust` exposes `load_package_verifier`, `check_entitlement`, and license-token installation. `apply_baseline`, `apply_rebaseline`, and `apply_incremental` verify the embedded manifest signature, run compatibility gates, run entitlement, verify payload digests, and only then mutate data.

`corpus_status` currently selects `corpus`, `active_generation`, `sequence`, `baseline_id`, `schema_version`, and `last_package_id`. It does not expose `embedding_fingerprint`, `builder_versions`, or `last_package_digest`, all of which already exist in `jurisearch_control.corpus_state`.

## Q1: Contract Extensions

**GO, with two additional fields.** Add the proposed fields:

- `CatchupPolicy.max_cumulative_uncompressed_to_baseline_permille`
- `CatchupPolicy.max_apply_seconds_budget`
- `BaselineRef.uncompressed_size_bytes`
- `BaselineRef.estimated_load_seconds`

Also add one of these before P7:

- preferred: `BaselineRef.package_kind: PackageKind`
- acceptable alternative: a public `apply_media_auto(...)` helper that reads the embedded manifest kind and dispatches to baseline vs re-baseline internally

Without that, `FreshBaseline(BaselineRef)` is underspecified. A fresh client may be applying the current active re-baseline, and an installed long-offline client must apply a re-baseline-style forward supersession, not a first-baseline package. Calling `apply_baseline` blindly will reject an installed corpus; calling `apply_rebaseline` blindly will reject a true first baseline.

I would also add `BaselineRef.minimum_client_version`. Otherwise the planner can return `FreshBaseline` for a baseline artifact the current binary cannot apply; the client only discovers it after fetch/apply. The apply gate still remains authoritative, but planning should be able to produce `Blocked { ClientTooOld }` before downloading when the signed remote manifest has the data.

Adding fields to signed manifest types has no special canonicalisation risk. The canonicaliser sorts object keys and handles integer fields deterministically. The practical cost is fixture churn and a wire-version bump expectation. Since this is still the implementation ladder and not an external stable wire protocol, prefer required fields over optional defaults for fields the planner needs to make §9.4 decisions.

## Q2: Planner Algorithm

**ADJUST.** The overall decision model is right, but tighten the exact ordering and cases.

Recommended pure planning flow:

1. Treat the `RemoteManifest` as already signature-verified. The pure planner should not accept unsigned remote data.
2. If there is no cursor, return `FreshBaseline(active_baseline)` after checking the baseline is apply-compatible enough to plan.
3. If `cursor.sequence == manifest.head_sequence`, return `UpToDate`. Do not force a newer-looking baseline at the same sequence; in this model a re-baseline advances the package sequence, so a same-sequence baseline should not represent new state.
4. If `cursor.sequence > manifest.head_sequence`, return `Blocked { WrongGeneration }` or equivalent. The client is ahead of the remote feed/environment.
5. If `cursor.sequence < manifest.min_available_sequence`, return `FreshBaseline`, but only if `active_baseline.sequence > cursor.sequence` for an installed client. If not, the remote manifest cannot catch this client up.
6. Build the chain by repeatedly finding exactly one package with `from_sequence == current` and `to_sequence == current.next()` until head. Missing, duplicate, or non-monotonic entries mean `FreshBaseline`.
7. If any relevant `catchup_ranges` entry says `RequiresBaseline` for the client position, prefer baseline even if the package list appears reconstructable. Use the package list as the authoritative apply list, but do not ignore an explicit signed routing range.
8. If any chain entry has `minimum_client_version > CLIENT_VERSION`, return `Blocked { ClientTooOld }`.
9. If any chain entry has `schema_version > CURRENT_SCHEMA_VERSION`, prefer baseline only if the active baseline is compatible with the current client schema/version; otherwise return `Blocked { SchemaAhead }` / client-upgrade-required. A too-new schema cannot be fixed by downloading a too-new baseline.
10. Prefer baseline if any chain entry has `requires_baseline`, differs from the cursor fingerprint/builders, exceeds any byte/time threshold, or makes chain length exceed `max_incremental_packages`.
11. Otherwise return `Incremental(chain)`.

For the reissue check, compare the first retained incremental's compatibility stamps against the current cursor stamps. If an entry differs from the cursor's `embedding_fingerprint` or `builder_versions`, route to baseline. This requires the P7 cursor to include those fields from `jurisearch_control.corpus_state`.

Do the ratio checks with integer arithmetic, not floats:

```text
cumulative_compressed * 1000 > baseline_compressed * max_compressed_permille
cumulative_uncompressed * 1000 > baseline_uncompressed * max_uncompressed_permille
```

Use `u128` or saturating arithmetic for the products. Define the zero-baseline-size case explicitly: if the baseline size is zero and cumulative diff is positive, prefer baseline or block as malformed manifest; do not divide by zero or silently choose incremental.

The `now` parameter does not belong in the pure planner as proposed. Entitlement expiry is enforced by `check_entitlement` against installed signed tokens and the DB clock, and remote manifest planning has no expiry field today. Keep planner pure over manifest + cursor + client version unless you add a freshness/SLA rule.

## Q3: CatchupSource and Module Placement

**GO, with a slightly wider fetch contract.** `CatchupSource` is the right seam for P7. Keep it in `jurisearch-syncd`; no new crate is justified yet because planning is consumer orchestration around local cursor state, trust, fetch, and apply.

Do not make the trait only `fetch(&str)`. P7 needs to bind the fetched bytes to the signed remote entry. Prefer one of:

```rust
fn fetch_package(&self, entry: &RemotePackageEntry) -> Result<PathBuf, _>;
fn fetch_baseline(&self, baseline: &BaselineRef) -> Result<PathBuf, _>;
```

or keep `fetch(uri)` but make `run_catchup` immediately verify the fetched artifact against the corresponding signed `sha256` from the entry/ref before calling apply.

For `FreshBaseline`, either:

- use `BaselineRef.package_kind` to call `apply_baseline` or `apply_rebaseline`; or
- add a public `apply_media_auto(client, artifact_dir, verifier)` that verifies the embedded manifest and dispatches based on `PackageKind`.

The second option reduces remote-manifest coupling, but the first option makes planning/debug output clearer. I would add the field and still consider an internal auto-dispatch helper to avoid duplicated manifest reads.

## Q4: Reference Apply-Cost Model

**GO.** For P7, use producer-declared `estimated_apply_seconds` and `estimated_load_seconds`. They are signed remote-manifest fields, tunable by corpus, and match §9.4's “reference client profile” model. The client should not invent a local cost model now.

Use the client only to enforce hard gates it actually knows locally: `CLIENT_VERSION`, `CURRENT_SCHEMA_VERSION`, installed cursor stamps, and later entitlement/trust. Calibration from measured apply timings can land in P10 and feed the producer's remote-manifest generation. P7 should record enough in tests to prove policy flips are data-driven, not client-hard-coded.

Use `u64` for sums even though each entry is `u32`; long retained chains can otherwise overflow a `u32` sum.

## Q5: `min_available_sequence` Boundary

**GO.** `requires_baseline_for(client_seq) == client_seq < min_available_sequence` is the right predicate.

That means `cursor.sequence == min_available_sequence - 1` routes to baseline. This matches the chain model in the source: an incremental applies when `current == entry.from_sequence`, and advances to `entry.to_sequence`. If the earliest retained start point is `min_available_sequence`, a client one sequence before that cannot start the retained chain.

If `cursor.sequence == min_available_sequence`, the client can attempt the retained incremental chain beginning with an entry whose `from_sequence == min_available_sequence`.

## Q6: Apply/Cursor/Trust Loop Risks

`apply_incremental` already enforces in-order application with `SequenceGap`, checks previous package identity/digest, validates active generation/baseline preconditions, and advances the cursor only after postcondition digests. The loop can rely on those as the final guard.

Still, the loop should validate ordering before fetching/applying so failures are deterministic and cheap:

- sort by `from_sequence`;
- require no duplicate `from_sequence`;
- require each `from_sequence == current` and `to_sequence == current.next()`;
- stop on the first apply failure, with no skip/retry of later entries;
- treat `AlreadyApplied` in the middle as acceptable only if the cursor has advanced exactly to that package's `to_sequence` with matching identity; otherwise let `apply_incremental` reject.

Remote trust layering should be:

1. Verify `Signed<RemoteManifest>` with `load_package_verifier` before planning.
2. Plan from the verified manifest.
3. Fetch each artifact through `CatchupSource`.
4. Verify the fetched artifact digest against the signed remote entry/ref.
5. Call the existing apply function, which re-verifies the embedded manifest, entitlement, payload files, and postconditions.

Do not treat planner pre-filtering as a substitute for apply gates. Entitlement is especially important here: the remote manifest can be filtered or user-friendly, but `check_entitlement` on the embedded manifest remains authoritative.

## Additional Gaps to Cover

Extend or add a cursor type. The proposed `ClientCursor` should include:

```rust
corpus
sequence
active_generation
baseline_id
schema_version
embedding_fingerprint
builder_versions
last_package_id
last_package_digest
```

The existing `CorpusStatus` is not enough for P7 reissue detection or debug-quality gap reporting.

Decide whether `catchup_ranges` is advisory or authoritative. My recommendation: signed `RequiresBaseline` ranges are authoritative for routing to baseline, while `IncrementalOk` ranges are advisory and must still be proven by reconstructing the concrete package chain.

Add manifest sanity checks. A remote manifest with `active_baseline.sequence > head_sequence`, duplicate package edges, package entries outside the corpus, or a head that cannot be reached should be rejected or routed baseline with a clear `Blocked` reason. Do not let malformed remote data become a strange apply error after downloads.

Make baseline compatibility explicit. If planner returns `FreshBaseline`, it should have checked at least baseline `minimum_client_version` and `schema_version`. Apply remains authoritative, but P7's point is to avoid doomed downloads when the signed remote manifest already proves the client cannot apply the artifact.

Update all remote manifest fixtures. `remote.rs` tests and `contract_acceptance.rs` construct `BaselineRef` and `CatchupPolicy` directly, so the new required fields will break compilation until fixtures are updated.

## Bottom Line

Implement P7 in `jurisearch-syncd` with a pure planner plus an orchestration loop. Make the missing remote contract fields explicit now, especially baseline package kind, uncompressed/load metrics, and baseline minimum client version. Add a planner cursor that reads the full `corpus_state` stamps. Keep every remote decision advisory until the existing P6/P4/P5 apply gates verify the actual artifact and move the cursor.
