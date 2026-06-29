# Codex review r3: secondary storage/core/embed splits

## Findings

No findings.

## Checks

- Confirmed the r3 fix commit narrows only the four core schema split helpers from `pub(crate)` to `pub(super)`:
  - `crates/jurisearch-core/src/schema/search.rs:7`
  - `crates/jurisearch-core/src/schema/admin.rs:7`
  - `crates/jurisearch-core/src/schema/eval.rs:7`
  - `crates/jurisearch-core/src/schema/gates.rs:7`
- Confirmed the only current textual uses of `schemas()` in the schema module are the parent-module assembly calls in `crates/jurisearch-core/src/schema/mod.rs:12-15`.
- Confirmed the latest commit's crates diff is limited to that visibility change for the four schema helpers; it does not alter schema bodies, `compiled_schema()`, command contract data, or public exports.
- Rechecked the reviewed crates diff for remaining over-wide visibility in the split areas. The remaining `pub(crate)` items I found are used for cross-module internals such as storage retrieval shared SQL/types and runtime SQL literal helpers; the r2 schema helper overexposure is resolved.
- Public API shape remains preserved: moved public items continue to be re-exported from the original public module roots, while the new implementation submodules remain private.
- Behavior preservation remains covered by the committed schema golden guard (`compiled_schema_matches_golden`). I did not rerun tests in this pass to avoid creating any files outside the requested review artifact.

VERDICT: GO
