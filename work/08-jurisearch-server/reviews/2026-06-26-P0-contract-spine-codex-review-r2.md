# P0 Contract Spine And Corpus Attribution Review R2

## Summary

I reviewed the current working tree, including the untracked `crates/jurisearch-package/` crate, the storage v18 corpus-attribution migration and writers, the updated storage/CLI tests, the r1 review, and the Phase 0 plan/design contracts. I did not rerun the already-reported validation suite; this review is source-grounded against the current diff.

The r1 fixes are mostly present: `ReplaceSet` now carries row bodies, sequence types are unsigned and non-interchangeable, `ReplaceSetScope` uses `Corpus`, catch-up policy no longer uses a raw float, and the storage writers now derive `corpus` in SQL with no runtime single-corpus fallback. However, the non-document-owned citation-resolution attribution is still not fail-loud once a deduped citation key already exists, and the signed-manifest wrapper still cannot use a runtime-selected `dyn Verifier`. Verdict remains FIXES_REQUIRED.

## BLOCKER

### 1. Existing citation resolutions can become cross-corpus ambiguous without failing

`legislation_citation_resolutions` is still keyed only by `citation_key` (`crates/jurisearch-storage/src/migrations.rs:676-689`). The v18 backfill derives `corpus` from occurrences with `HAVING count(DISTINCT d.corpus) = 1` and then makes the column `NOT NULL` (`crates/jurisearch-storage/src/migrations.rs:782-800`), and the runtime insert does the same derivation for a new row (`crates/jurisearch-storage/src/legislation_citations.rs:128-136`). That fixes the first insert for a key.

The bug is the conflict path: after inserting a later `decision_legislation_citations` occurrence, `upsert_citation_resolution_pending_with_client` uses `ON CONFLICT (citation_key) DO NOTHING` (`crates/jurisearch-storage/src/legislation_citations.rs:112-136`). If a key was first seen in `core` and is later seen in another corpus, the occurrence insert succeeds, the resolution upsert skips the derivation entirely, and the existing resolution row remains attributed to the original corpus. That contradicts the stated P0 contract that citation-resolution corpus is derived from authoritative occurrences and ambiguous rows fail loudly. It also creates a per-corpus packaging hazard: the later corpus can have occurrence rows sharing a resolution/Legifrance archive that is only attributed to the earlier corpus.

The current added test covers two decisions in the same corpus (`crates/jurisearch-storage/tests/legislation_citations.rs:67-150`), but it does not exercise the conflict path or an ambiguity after the first resolution row exists.

Recommended fix: make the resolution ownership invariant hold after every occurrence insert, not only on first resolution insert. The cleanest package-compatible shape is to scope resolutions by corpus, e.g. add `corpus` to the key (`PRIMARY KEY (corpus, citation_key)` or equivalent unique constraint) and make all pending/load/update paths address `(corpus, citation_key)`. If the intended Phase 0 contract is still "one global citation_key must never span corpora", add a trigger or writer-side validation after each occurrence insert that raises when `count(DISTINCT corpus) != 1` for that key, and change the conflict path to revalidate instead of `DO NOTHING`. Add an integration test that inserts an existing citation key through one corpus, then inserts an occurrence for the same key through a second corpus, and proves the write fails loudly or creates a separate per-corpus resolution.

## WARN

### 1. `Signed<T>` still hides the object-safe verifier behind a concrete `impl` API

The `Signer`/`Verifier` traits were made object-safe at the core method level: `sign_bytes`/`verify_bytes` are object-safe, while the generic helpers are `where Self: Sized`, and the free functions accept `&(impl Signer + ?Sized)` / `&(impl Verifier + ?Sized)` (`crates/jurisearch-package/src/crypto.rs:44-112`). That resolves the trait definitions themselves.

But the primary signed-document helper still uses `pub fn seal(payload: T, signer: &impl Signer)` and `pub fn verify(&self, verifier: &impl Verifier)`, then calls the `Self: Sized` generic helpers (`crates/jurisearch-package/src/signed.rs:20-36`). Because `&impl Trait` keeps the hidden type `Sized`, callers cannot pass `&dyn Verifier` to `Signed<T>::verify`, which is the obvious API future client code will use for the signed remote and embedded manifests. The test only verifies `&dyn Verifier` via the free function in `crypto.rs`, not through `Signed<T>`.

Recommended fix: change `Signed::seal` and `Signed::verify` to accept `&(impl Signer + ?Sized)` / `&(impl Verifier + ?Sized)` and call `crate::crypto::sign_value` / `crate::crypto::verify_value` rather than the `Self: Sized` trait helpers. Add a compile-time/unit regression that `let verifier: &dyn Verifier = &AcceptAllVerifier; signed.verify(verifier)` compiles, and the same for `Signed::seal` with `&dyn Signer`.

## R1 Findings Check

- BLOCKER 1 (`replace_set` row bodies): resolved. `ReplaceSet.rows` is present and round-tripped in tests (`crates/jurisearch-package/src/event.rs:106-160`).
- BLOCKER 2 (corpus attribution on non-document-owned tables): partially resolved but still blocked by the existing-resolution conflict path described above.
- WARN 1 (sequence domain): resolved. `ChangeSeq` and `PackageSequence` are `u64`-backed and reject negative wire values.
- WARN 2 (compat schema gate): resolved. The old all-in-one helper is split into `check_fingerprint_and_builders` plus `schema_gate`.
- WARN 3 (`dyn Signer`/`Verifier`): partially resolved. The traits/free functions are object-safe, but `Signed<T>` is not.
- NIT 1 (`ReplaceSetScope` corpus type): resolved. The field is `Option<Corpus>`.
- NIT 2 (`CatchupPolicy` float): resolved for the signed wire format. The policy now uses an integer per-mille field.

VERDICT: FIXES_REQUIRED
