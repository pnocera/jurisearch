# P6 Trust & Gating Design Validation

## Verdict

**ADJUST, then build.** The proposed shape is mostly right: asymmetric Ed25519, DB-backed client trust state in `jurisearch_control`, keeping `Signer`/`Verifier` as the seam, and running all trust/gating before row mutation fit the current code. The main corrections are:

- Do **not** verify `artifact_sha256` from the embedded manifest before verifying the embedded manifest signature. That field is itself signed data.
- Do **not** model `license_token` as `PRIMARY KEY (corpus)` unless the product guarantees one token per corpus forever. Use a key that can hold multiple tiers/audiences/epochs.
- Do **not** treat `canonical_digest(manifest)` as an internal integrity check unless there is an external expected manifest digest to compare against. Today it only proves canonicalisation succeeds; the signature already covers canonical bytes.
- Stop hard-coding `integrity.signature_algorithm = "stub"` in the builders once real signing is enabled, or make it explicitly non-authoritative. The authoritative algorithm is already in `Signed.signature.algorithm`.

## Source Constraints Verified

The existing seams are good for P6. `crates/jurisearch-package/src/crypto.rs` has object-safe `Signer`/`Verifier` primitives, `Signature { algorithm, key_id, key_epoch, signature_hex }`, canonical `sign_value`/`verify_value`, and test-only `StubSigner`/`AcceptAllVerifier`. `Signed<T>` in `signed.rs` signs/verifies the canonical payload.

The apply path is already close to the intended trust shape. `apply_baseline`, `apply_rebaseline`, and `apply_incremental` all accept `&dyn Verifier` and verify `Signed<EmbeddedManifest>` before compatibility gates and payload-file digests. `verify_per_file_digests` recomputes every payload file digest, requires exact equality with `integrity.per_file_digests`, then recomputes the aggregate payload digest and compares it to both `artifact_sha256` and `uncompressed_payload_digest`.

The builders already accept `&dyn Signer` and seal manifests through `Signed::seal`. Baseline/re-baseline and incremental currently set `integrity.signature_algorithm` to `"stub"` in the manifest body, while the real signature algorithm is carried by the outer `Signed.signature`. That placeholder must be cleaned up during P6.

`jurisearch-syncd/src/main.rs` still wires `AcceptAllVerifier` directly. That is fine for P3-P5 tests, but P6 must make the production binary build an Ed25519 verifier from client trust anchors and reserve `AcceptAllVerifier` for tests/loopback-only commands.

## Q1: Concrete Crypto

**GO with `ed25519-dalek` 2.x.** Ed25519 is the right primitive here: producer signs, clients verify; no shared secret exists on the client; signatures are small and deterministic; and the existing trait already abstracts the byte-level signing/verification operation.

I would add concrete types under `jurisearch_package::crypto`, or a small `crypto::ed25519` submodule:

- `Ed25519Signer { signing_key, key_id, key_epoch }`
- `Ed25519Verifier { trust_anchors: BTreeMap<(KeyId, KeyEpoch), VerifyingKey> }`

Return the existing `VerifyError` variants: `UnsupportedAlgorithm` when `signature.algorithm != "ed25519"`, `UnknownKey` when the `(key_id, key_epoch)` anchor is absent, and `Invalid` when the signature does not verify.

Adding `ed25519-dalek` is acceptable even though the workspace currently only uses `sha2`. You will probably also want either a tiny `hex` dependency or local strict hex encode/decode helpers. If you add `hex`, use strict lower-case output and reject malformed or wrong-length public keys/signatures: Ed25519 public key = 32 bytes, signature = 64 bytes.

Do not switch to HMAC or any shared-key scheme. `ring` would also be defensible, but it does not fit the current trait any better than `ed25519-dalek`, and Ed25519 is simpler for this package-contract layer.

## Q2: Trust Store and License Tokens

**GO on `jurisearch_control`, ADJUST the token schema.** `jurisearch_control` is the right home because trust anchors and entitlements are local client control state, must survive generation swaps/re-baselines, and are naturally read by `syncd` next to `corpus_state`. They are not replicated corpus data and must not live in a generation schema.

I would still keep an import/export/config path later, because trust anchors are deployment configuration. But the durable source of truth for the running client can be the control DB.

Recommended migration v23 shape:

```sql
CREATE TABLE jurisearch_control.trust_anchor (
    key_id text NOT NULL,
    key_epoch integer NOT NULL,
    algorithm text NOT NULL,
    public_key_hex text NOT NULL,
    purpose text NOT NULL DEFAULT 'package',
    installed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (key_id, key_epoch, purpose)
);

CREATE TABLE jurisearch_control.license_token (
    corpus text NOT NULL,
    tier text NOT NULL,
    license_epoch integer NOT NULL,
    audience text,
    not_after text,
    token_json jsonb NOT NULL,
    installed_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (corpus, tier, license_epoch, audience)
);
```

The `purpose` column is optional but worth adding now. It lets you distinguish package-signing anchors from license-signing anchors later without a schema redo. If you want one trust set for P6, permit `purpose IN ('package','license','both')` or duplicate rows by purpose.

Do not use `PRIMARY KEY (corpus)` for license tokens. It blocks normal cases such as multiple tiers, staged license epochs, different audiences, or an installed renewal before the old token expires. Entitlement checking should select any valid token covering `(corpus, tier, audience)` with `license_epoch >= package.entitlement.license_epoch`.

The license token type belongs in `jurisearch-package`, not storage, so producer/licensing and client agree on canonical signing:

```rust
LicenseToken {
    entitlement_corpus,
    tier,
    license_epoch,
    audience,
    not_after,
}
```

For `not_after`, either enforce a canonical UTC RFC3339 string before storing or defer expiry. Do not compare arbitrary strings as dates. For P6, a small parser/validator is enough; no need for a full policy engine.

## Q3: Ordered Verifier Pipeline

**ADJUST the order.** The current local apply path already does the most important thing correctly: embedded manifest signature first, then signed compatibility and payload digest checks. Preserve that.

Recommended pipeline:

1. Parse the signed embedded manifest, but do not trust its fields yet.
2. For network only: verify `Signed<RemoteManifest>` first with the trust store.
3. For network only: use the signed remote manifest entry to check package identity, sequence, corpus, expected digest, and advertised signing metadata before applying the downloaded package. Media apply skips this step.
4. Verify `Signed<EmbeddedManifest>` with the Ed25519 verifier.
5. Verify embedded-manifest consistency that does not require an external expected digest: package kind, corpus identity, sequence shape, and supported canonicalisation/signature algorithm fields.
6. Run version/schema/extension gates and entitlement gate before row mutation.
7. Verify per-file digests and the aggregate payload digest from bytes actually read.
8. Apply rows in the existing baseline/rebaseline/incremental transaction shapes.
9. Validate postcondition digests in-transaction before cursor movement.

The proposed step `(b) artifact digest` before `(c) embedded-manifest signature` is only valid if the expected digest comes from the already verified remote manifest. It is not valid for media/local apply when the expected digest is `manifest.integrity.artifact_sha256`, because that value is inside the unsigned payload until step 4 succeeds.

Also remove or rename the proposed “embedded-manifest internal digest” step. `canonical_digest(manifest)` currently has no expected value inside the embedded manifest. By itself it only proves the manifest can be canonicalised. If you compare it to the producer catalog or a remote manifest field, then it is a real digest check; otherwise map only canonicalisation failure to `DigestMismatch` and do not call it integrity.

Reject-code mapping is otherwise right:

- Signature failures, unknown keys, unsupported algorithms: `SignatureInvalid`
- Payload/manifest/postcondition digest mismatch: `DigestMismatch`
- Client too old: `ClientTooOld`
- Schema mismatch/ahead: `SchemaAhead`
- Missing required extension: `ExtensionMissing`
- No valid installed subscription token: `MissingEntitlement`

For an invalid installed license token during package apply, treat it as “no valid entitlement” and return `MissingEntitlement`. During `subscribe <corpus>` token installation, invalid token signature should be `SignatureInvalid`.

## Q4: Artifact Digest

**GO for P6, but name the limitation honestly.** The current code already defines `artifact_sha256` as the aggregate over verified payload-file digests, and `verify_per_file_digests` recomputes that aggregate from bytes read off disk. That is sufficient for the current directory-artifact/media path and preserves P3-P5 behavior.

Do not claim this is a tarball or transport-object hash until P9 introduces real artifact transport. For P6, either:

- keep using aggregate payload digest for `integrity.artifact_sha256`, with a code comment that this is the logical artifact digest for unpacked artifacts; or
- add a separate field later for whole-archive digest and leave the current aggregate as payload integrity.

If P6 adds remote manifest tests now, make the remote entry `sha256` explicitly equal the same aggregate payload digest for unpacked test artifacts. Real downloaded-byte hashing can land with transport.

## Q5: Rotation Tolerance

**GO.** Accepting any `(key_id, key_epoch)` present in `trust_anchor` is sufficient for P6. The producer signs with its current epoch; the client accepts epochs still installed in its trust store. Removing an old anchor is the P6 revocation/floor mechanism.

I would not add a mandatory epoch-floor policy yet unless the plan already has a concrete ops model for it. If you add anything now, make it small and explicit:

- `trust_anchor.algorithm` must match `signature.algorithm`.
- malformed key material fails verifier construction.
- duplicate anchors for the same `(key_id, key_epoch, purpose)` are impossible by PK.
- the production verifier must not fall back to `AcceptAllVerifier` when the trust store is empty.

An epoch floor can be added later as either a trust-store policy table or by deleting old anchors.

## Q6: Wiring Risks

The existing structure is friendly to P6. Builders already take `&dyn Signer`; appliers already take `&dyn Verifier`; tests already use `StubSigner`/`AcceptAllVerifier`. Keep those tests working, and add dedicated real-crypto tests rather than converting every data-path loopback test to Ed25519.

Concrete wiring recommendations:

- Add real signer/verifier implementations without changing `Signed<T>` or the manifest wire types.
- Replace `jurisearch-syncd/src/main.rs` production use of `AcceptAllVerifier` with a `load_verifier_from_trust_store(&ManagedPostgres, purpose)` helper.
- Keep `AcceptAllVerifier` available only in tests or explicitly named unsafe loopback helpers.
- Extract a shared `verify_embedded_artifact(...)` helper used by baseline, re-baseline, and incremental apply so P6 does not create three divergent verifier pipelines.
- Make builders set `integrity.signature_algorithm` from the signer algorithm, or drop enforcement of that field and rely on `Signed.signature.algorithm`. The current hard-coded `"stub"` will otherwise make real Ed25519 manifests internally inconsistent.
- Clarify `RemotePackageEntry.signature` and `BaselineRef.signature`. Right now those are bare `Signature` values with no typed payload wrapper. Either define exactly what they sign (for example canonical entry-without-signature or artifact digest bytes) or defer their enforcement and rely on `Signed<RemoteManifest>` plus `Signed<EmbeddedManifest>` for P6. Do not add an ambiguous verification call.

## Entitlement Gate Detail

For P6, use a simple deterministic rule:

- If `manifest.entitlement.tier` is open-equivalent, no token is required. Current builders use `"all"` with `license_epoch = 0`; decide whether `"all"` means open for P6 or change builders to write `"open"` before introducing the gate.
- Otherwise find a valid signed `LicenseToken` whose corpus matches `entitlement_corpus`, tier covers the package tier, audience matches or is absent according to the chosen policy, `license_epoch >= manifest.entitlement.license_epoch`, and `not_after` is absent or in the future.
- Verify the token signature with a license-purpose trust anchor each time it is used, not only at install time. Stored `token_json` is local mutable state; re-verification is cheap and avoids trusting the DB row blindly.

Keep tier coverage exact-match in P6 unless you add a typed tier hierarchy. String tiers plus implicit hierarchy are a future bug.

## Rework Triggers If Built As Proposed

These would force cleanup later if not corrected now:

- `license_token PRIMARY KEY (corpus)`: too narrow for renewals, audiences, and tier changes.
- artifact digest checked against unsigned embedded-manifest fields before embedded signature verification.
- “canonical digest” treated as an integrity check with no external expected digest.
- production `syncd` continuing to use `AcceptAllVerifier`.
- real Ed25519 manifests still carrying `integrity.signature_algorithm = "stub"`.
- undefined semantics for `RemotePackageEntry.signature` / `BaselineRef.signature`.

With those adjustments, P6 is a good next slice: it hardens the package trust boundary without disturbing the P3-P5 storage, generation, and cursor mechanics.
