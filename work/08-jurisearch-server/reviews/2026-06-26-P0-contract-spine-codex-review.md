# P0 Contract Spine And Corpus Attribution Review

## Summary

Phase 0 adds the new `jurisearch-package` contract crate, wires it into the workspace, and bumps storage to schema v18 with a generated `documents.corpus` column. The contract crate covers much of the planned surface, and the focused checks I ran passed:

- `cargo fmt --check`
- `CARGO_TARGET_DIR=/tmp/codex-jurisearch-review-target cargo test -p jurisearch-package`
- `CARGO_TARGET_DIR=/tmp/codex-jurisearch-review-target cargo test -p jurisearch-storage migrations::tests`

However, two P0 acceptance criteria are not actually met against the design and existing schema: the typed `replace_set` payload cannot carry or round-trip the design's rebuilt rows, and corpus attribution does not cover all replicated rows, especially official API archive and citation-resolution rows that have no owning document FK. Verdict is FIXES_REQUIRED.

## BLOCKER

### 1. `ReplaceSet` drops the actual rebuilt rows required by the design payload

`crates/jurisearch-package/src/event.rs:106` describes `ReplaceSet` as the section 5.3 payload contract and says it carries "the rebuilt rows", but the struct at `event.rs:112-129` only contains `op`, `table_group`, `scope`, `row_pks`, builder/fingerprint stamps, and `set_digest`. The design's section 5.3 example includes a required `rows` object with the authoritative `zone_units` and `zone_unit_embeddings` row bodies. With the current default serde behavior, a real design-shaped JSON payload containing `rows` would deserialize by ignoring that unknown field, then reserialize without the row bodies. That fails the P0 acceptance criterion that the crate round-trip the design event examples, and it leaves the future applier without typed access to the rows it must delete/reinsert/verify.

Recommended fix: add a wire type that represents the full `replace_set` event payload, including the row bodies. A pragmatic first version can use a deterministic `BTreeMap<String, Vec<serde_json::Value>>` for `rows`, or a group-specific enum if you want stronger typing now. If `ReplaceSet` is intended to be only a scope/digest descriptor and row bodies live exclusively in payload files, split that into a separate descriptor type and add the missing package event-file type that round-trips the section 5.3 example. In either case, add an acceptance test that parses the actual design-shaped `replace_set` JSON including `rows`, canonicalizes it, serializes it back, and proves the row payload is preserved.

### 2. Corpus attribution is incomplete for replicated rows without an owning document

P0 requires every replicated row to resolve to exactly one corpus, with ambiguous rows failing loudly. The migration only adds `documents.corpus` (`crates/jurisearch-storage/src/migrations.rs:717-727`) and then claims derived/dependent tables, including `official_api_responses` and citations, inherit through an owning document (`migrations.rs:705-710`). That is not true for the current schema. `official_api_responses.subject_document_id` is nullable (`migrations.rs:593-602`), and the Legifrance citation resolver writes archive rows with `subject_document_id = None`, `subject_source_uid = None`, and only `citation_key` set (`crates/jurisearch-cli/src/enrichment/legislation.rs:343-350`). `legislation_citation_resolutions` is keyed by `citation_key` and only optionally links to that archive row via `legifrance_response_id` (`migrations.rs:676-684`), so neither table has a guaranteed document-owned corpus. Those rows can enter the replicated set without the v18 check ever attributing or rejecting them.

Recommended fix: make corpus attribution explicit on non-document-owned replicated tables. At minimum, add `corpus text NOT NULL` with a validated/backfilled value to `official_api_responses` and `legislation_citation_resolutions`, and update `InsertOfficialApiResponse` plus citation-resolution writers to require a `Corpus` instead of allowing an unattributed archive insert. For citation resolutions, either scope the key as `(corpus, citation_key)` or add a backfill/check that derives corpus from associated occurrences and fails if a citation key spans multiple corpora. Keep `decision_legislation_citations` document-derived if desired, but add a test that exercises the current Legifrance citation path and proves the archived response and resolution both resolve to exactly one corpus.

## WARN

### 1. Sequence newtypes allow invalid negative wire values

`ChangeSeq::new` and `PackageSequence::new` accept any `i64` (`crates/jurisearch-package/src/sequence.rs:32-35`, `sequence.rs:57-60`), and `PackageSequence::predecessor()` returns `self.0 - 1` (`sequence.rs:73-78`). Because both types are `serde(transparent)`, a manifest can currently deserialize negative sequence coordinates even though `change_seq` is a `bigserial` and package sequence is a non-negative, gap-free cursor with `NONE = 0`. Calling `PackageSequence::NONE.predecessor()` produces `-1`, an impossible client cursor.

Recommended fix: make these constructors validate the domain. Either use `u64` for both sequence wrappers, or provide fallible `try_new` constructors and serde `try_from` validation that reject negative values. Change `predecessor()` to return `Option<PackageSequence>` or `Result<_, _>` at zero, and add manifest tests for rejecting negative `from_sequence`, `to_sequence`, `head_sequence`, and `min_available_sequence`.

### 2. The compatibility helper skips the schema precondition while presenting itself as the incremental check

`CompatibilityStamps` includes `schema_version`, but `check_incremental_against` explicitly does not compare it (`crates/jurisearch-package/src/compat.rs:27-38`) and then only checks fingerprint and builder versions (`compat.rs:43-71`). The design's embedded manifest apply contract lists schema version as a precondition, and P0 includes schema version in the compatibility stamp set. As written, downstream code can reasonably call this helper as the compatibility gate and accidentally accept a package/cursor schema mismatch.

Recommended fix: either rename/split the helper so it is clearly only a fingerprint/builder check, or add an explicit schema gate to the contract crate. The schema gate should model the intended cases: DB ahead of binary rejects with `schema_ahead`, package schema equal applies normally, package schema one or more additive steps ahead requires the declared migration bundle before row apply, and incompatible/breaking schema requires baseline/rebaseline handling.

### 3. `Signer` and `Verifier` are not usable as trait objects

The trust boundary is meant to be a swappable `Signer`/`Verifier` abstraction, but both traits include generic helper methods (`sign_value<T: Serialize>` at `crates/jurisearch-package/src/crypto.rs:48` and `verify_value<T: Serialize>` at `crypto.rs:67`). That makes the traits not dyn-compatible, so a future service cannot hold `Box<dyn Verifier>` or inject a runtime-selected verifier without wrapping it again.

Recommended fix: keep `sign_bytes` and `verify_bytes` as the object-safe core methods, and add `where Self: Sized` to the generic helper methods or move the helpers to extension traits/free functions. Add a small compile-time/unit check that a `&dyn Verifier` can be passed through the verification surface.

## NIT

### 1. `ReplaceSetScope` bypasses the validated `Corpus` type

`ReplaceSetScope.corpus` is `Option<String>` (`crates/jurisearch-package/src/event.rs:139-145`), even though the crate already has a validated `Corpus` newtype for schema/license-safe corpus tokens. This leaves one package-facing corpus field outside the central validation rule.

Recommended fix: change the field to `Option<Corpus>` and update tests accordingly. If the wire needs to stay a string, `Corpus` already serializes transparently while preserving validation on deserialize.

### 2. The signed remote catch-up policy uses a raw `f64`

`CatchupPolicy.max_cumulative_diff_to_baseline_ratio` is an `f64` (`crates/jurisearch-package/src/manifest/remote.rs:117-121`). It is part of a signed, canonicalized manifest, but raw floats are a wider domain than the contract needs and make range validation easy to skip.

Recommended fix: represent the threshold as an integer basis-point or per-mille value, or add a small validated decimal wrapper that rejects NaN/Inf and values outside the allowed range. That keeps the signed wire format deterministic and makes the policy domain explicit.

VERDICT: FIXES_REQUIRED
