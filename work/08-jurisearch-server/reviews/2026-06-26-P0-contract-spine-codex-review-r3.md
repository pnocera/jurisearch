# P0 Contract Spine And Corpus Attribution Review R3

## Summary

I reviewed the current working tree, including the uncommitted storage/CLI changes and the untracked
`crates/jurisearch-package/` crate. I focused on the two r2 findings, then checked the surrounding
P0 contract surface for regressions introduced by the fixes.

Verdict is GO. I did not find any remaining blocker or warning-level issue.

## Findings

No findings.

## R2 Findings Check

### BLOCKER: citation-resolution cross-corpus ambiguity

Resolved.

Migration v18 now re-keys `legislation_citation_resolutions` by `(corpus, citation_key)` after adding
and backfilling the explicit `corpus` column (`crates/jurisearch-storage/src/migrations.rs:779-811`).
The runtime pending-resolution writer derives corpus from the current occurrence's
`decision_document_id -> documents.corpus` and conflicts on `(corpus, citation_key)`, so the second
corpus to cite the same normalized article creates its own resolution row rather than inheriting the
first corpus's row (`crates/jurisearch-storage/src/legislation_citations.rs:114-143`).

The dependent paths were also threaded through the composite identity:

- occurrence counts are recomputed per `(corpus, citation_key)` (`crates/jurisearch-storage/src/legislation_citations.rs:146-159`);
- pending-resolution paging returns `corpus` and keysets on `(corpus, citation_key)` (`crates/jurisearch-storage/src/legislation_citations.rs:161-214`);
- resolution updates are scoped by both fields (`crates/jurisearch-storage/src/legislation_citations.rs:216-238`);
- the CLI collect path passes the decision document id into the upsert (`crates/jurisearch-cli/src/enrichment/legislation.rs:254-262`);
- the CLI enrich path reads the resolution's corpus, archives the Legifrance response with that explicit corpus, and updates the matching `(corpus, citation_key)` row (`crates/jurisearch-cli/src/enrichment/legislation.rs:322-388`);
- `official_api_responses` derives runtime corpus only from an explicit corpus or the subject document, no longer from `citation_key` (`crates/jurisearch-storage/src/official_api_archive.rs:63-105`).

The new regression test directly proves two rows with the same `citation_key` can coexist under
different corpora and are both visible through the pending pager
(`crates/jurisearch-storage/tests/legislation_citations.rs:208-257`).

### WARN: `Signed<T>` not dyn-usable

Resolved.

`Signed::seal` and `Signed::verify` now accept `&(impl Signer + ?Sized)` /
`&(impl Verifier + ?Sized)` and dispatch through the object-safe free functions
(`crates/jurisearch-package/src/signed.rs:20-42`, `crates/jurisearch-package/src/crypto.rs:89-112`).
The regression test exercises both `&dyn Signer` and `&dyn Verifier`
(`crates/jurisearch-package/src/signed.rs:79-91`).

## Additional P0 Checks

The earlier contract-spine issues remained fixed: `ReplaceSet` still carries row bodies and tests
round-trip them (`crates/jurisearch-package/src/event.rs:106-160`,
`crates/jurisearch-package/tests/contract_acceptance.rs:259-295`); `ChangeSeq` and
`PackageSequence` remain distinct `u64` newtypes (`crates/jurisearch-package/src/sequence.rs:35-92`);
the reject-code vocabulary remains closed and covered by acceptance tests
(`crates/jurisearch-package/src/reject.rs:17-104`,
`crates/jurisearch-package/tests/contract_acceptance.rs:297-313`).

## Verification

I ran:

```text
CARGO_TARGET_DIR=/tmp/codex-review-p0-r3-target cargo test --locked -p jurisearch-package -p jurisearch-storage --lib
CARGO_TARGET_DIR=/tmp/codex-review-p0-r3-target cargo test --locked -p jurisearch-cli --test cli_ingest_contract --no-run
```

Both passed. The target directory was outside the repository; the repo status after verification was
unchanged except for the pre-existing working-tree changes and this review file.

VERDICT: GO
