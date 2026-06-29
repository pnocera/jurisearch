# Codex Review: Phase 3a Leaf Helpers + Retrieval Split

## Findings

### NIT - `output.rs` module doc still points to the old error-helper owner

[crates/jurisearch-cli/src/output.rs:4](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/output.rs:4) says `ErrorObject` construction lives with helpers "currently in `main.rs`, moving to `errors.rs` in a later phase", but this diff has already moved those constructors and mappings into `errors.rs` and imports `dependency_unavailable` from there at line 16. This is documentation-only, but it can mislead the next refactoring pass about current ownership.

Recommended fix: update the comment to say `ErrorObject` construction lives in `errors.rs`.

### NIT - `retrieval/mod.rs` overstates `validate_as_of` consumers

[crates/jurisearch-cli/src/retrieval/mod.rs:3](/home/pierre/Work/jurisearch/crates/jurisearch-cli/src/retrieval/mod.rs:3) says `validate_as_of` is shared by cite/context and "in status, diff", but current references are only `retrieval/cite.rs` and `retrieval/context.rs`. Status/diff currently use the lower-level date validation path directly. This does not change behavior, but it makes the ownership note less precise.

Recommended fix: either remove the status/diff mention or wire those callers through `validate_as_of` in a separate behavior-neutral cleanup if that is desired.

## Review Notes

I reviewed `git diff 50f204d..HEAD -- crates/jurisearch-cli`, including the leaf helper extraction (`ascii.rs`, `date.rs`, `query_support.rs`, `legifrance_search.rs`, `errors.rs`, `citation.rs`, `embedding_runtime.rs`) and the retrieval command moves (`retrieval/search.rs`, `zone.rs`, `cite.rs`, `context.rs`, `related.rs`, `compare.rs`, `expand.rs`, `mod.rs`).

The moved retrieval payloads, citation-state logic, cursor parser, citation parser, error constructors, embedding runtime wrapper, and Legifrance body builder are behavior-preserving relative to the base. The visible changes are module declarations/imports, widened `pub(crate)` visibility needed for sibling modules/tests, and module comments. I did not find any change to CLI JSON payload shape, exit-code mapping, cursor validation, citation-state classification, strict-citation error construction, online citation confirmation behavior, or session/one-shot parity.

I did not rerun the full `cargo test -p jurisearch-cli` suite because the review request only required a source review and the brief already listed that validation as clean.

VERDICT: GO
