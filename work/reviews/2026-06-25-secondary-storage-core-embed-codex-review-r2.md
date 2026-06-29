# Codex review r2: secondary storage/core/embed splits

## Findings

### WARN: schema split helpers are still crate-visible even though only the parent module uses them

The visibility minimization pass missed the four `schemas()` helpers introduced by the core schema split. In `crates/jurisearch-core/src/schema/mod.rs`, the only call sites are the parent module assembling the schema map:

- `crates/jurisearch-core/src/schema/mod.rs:12` calls `search::schemas()`
- `crates/jurisearch-core/src/schema/mod.rs:13` calls `admin::schemas()`
- `crates/jurisearch-core/src/schema/mod.rs:14` calls `eval::schemas()`
- `crates/jurisearch-core/src/schema/mod.rs:15` calls `gates::schemas()`

But the helpers are declared `pub(crate)`:

- `crates/jurisearch-core/src/schema/search.rs:7`
- `crates/jurisearch-core/src/schema/admin.rs:7`
- `crates/jurisearch-core/src/schema/eval.rs:7`
- `crates/jurisearch-core/src/schema/gates.rs:7`

That is broader than their uses require. They only need to be visible to `schema/mod.rs`, so `pub(super)` is sufficient and keeps the split internals out of the rest of `jurisearch-core`.

Recommended fix: change all four declarations from `pub(crate) fn schemas()` to `pub(super) fn schemas()`, then rerun `cargo test -p jurisearch-core compiled_schema_matches_golden`.

## Round-1 Checks

- The accidental dataset sweep is resolved in the reviewed diff: `git diff --name-only 8cdcd79..HEAD -- work/07-datasets` returns no tracked changes. The directory is still present only as untracked working-tree content.
- The over-widened visibility issue is mostly resolved in the moved storage, ingest, embed, and official-api code. The remaining over-wide items I found are the four core schema `schemas()` helpers above.
- Public API shape appears preserved for the reviewed splits: public module roots still re-export the moved public types/functions at their original paths, and the newly introduced submodules are private implementation details.
- Behavior preservation for the schema split is guarded and still passes: `cargo test -p jurisearch-core compiled_schema_matches_golden` passed locally.

VERDICT: FIXES_REQUIRED
