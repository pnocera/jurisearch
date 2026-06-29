# Codex Review: Secondary Refactors Batch 2

## Findings

### WARN: The reviewed range contains unrelated dataset-download scripts inside the embed split commit

The stated scope is behavior-preserving module splits for ingest/juri, ingest/legi, storage retrieval/projection/accounting, core schema, and embed. However, `git diff 8cdcd79..HEAD` also adds `work/07-datasets/01-bofip.sh` through `work/07-datasets/09-dg-comp-eu.sh` plus `work/07-datasets/README.md`, and `git show --name-only 7317099` shows these files are bundled into `7317099 embed: split lib.rs into config/fingerprint/client/tokenizer/error (+ tests.rs)`.

These files are not refactor artifacts and are outside the reviewed behavior-preserving API-split contract. They also introduce new executable operational download behavior, including credential/setup guidance in `work/07-datasets/README.md:21`, so they should not ride along in a mechanical embed split.

Recommended fix: remove the `work/07-datasets/*` additions from this refactor batch, or move them to a separate, explicitly reviewed dataset-work commit/PR with its own validation.

### WARN: Internal helper visibility was widened to crate-wide across private split modules

Many items that were file-private before the split are now `pub(crate)` even though the new modules are private implementation modules and the call sites only need parent/sibling access. Representative examples:

- `crates/jurisearch-ingest/src/legi/parser.rs:5` and `crates/jurisearch-ingest/src/legi/parser.rs:8` expose parser-only constants/raw structs crate-wide.
- `crates/jurisearch-ingest/src/juri/inferred_citations.rs:7` and `crates/jurisearch-ingest/src/juri/inferred_citations.rs:12` expose regex internals crate-wide.
- `crates/jurisearch-storage/src/projection/hierarchy_backfill.rs:255` and `crates/jurisearch-storage/src/projection/hierarchy_backfill.rs:266` expose backfill selection internals crate-wide.
- `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:35` and `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:47` expose readiness internals crate-wide.
- `crates/jurisearch-embed/src/client.rs:5` exposes client constants crate-wide.

The retrieval split has a documented cross-module reason for `pub(crate)` re-exports because `zone_retrieval.rs` imports `DecisionFilters`, `RRF_K`, `document_cursor_predicate`, `effective_probes`, `effective_rrf_weights`, and `format_sql_f64` from `crate::retrieval` (`crates/jurisearch-storage/src/zone_retrieval.rs:14`, `crates/jurisearch-storage/src/retrieval.rs:32`). The same need is not apparent for the ingest/projection/accounting/embed helper internals above.

Recommended fix: downgrade implementation-only helper items in private submodules from `pub(crate)` to `pub(super)` where sibling/module-root access is needed, and keep them private where only the defining file uses them. Leave the retrieval helpers that `zone_retrieval` imports as `pub(crate)`.

### NIT: `git diff --check` fails on blank lines at EOF in the added dataset scripts

`git diff --check 8cdcd79..HEAD -- work/07-datasets` reports trailing blank-line-at-EOF errors in all nine new scripts, for example `work/07-datasets/01-bofip.sh:41`. This is separate from the module split and would fail a whitespace gate over the reviewed range.

Recommended fix: if the dataset scripts stay in any branch, remove the extra blank line after each final `EOF`.

## Notes

I did not find evidence that the intended public crate exports were dropped: the hub files still re-export the prior public surfaces for `jurisearch_ingest::legi`, `jurisearch_ingest::juri`, `jurisearch_storage::{retrieval,projection,ingest_accounting}`, `jurisearch_core::schema::compiled_schema`, and `jurisearch_embed::*`.

The core schema split keeps `compiled_schema()` at `jurisearch_core::schema::compiled_schema`, assembles a single flat `schemas` map, and includes the new golden test at `crates/jurisearch-core/src/schema/mod.rs:124`. `cargo tree -e features -i serde_json --prefix none` shows `serde_json` default/std features only, so the sorted-map assumption used by the regrouped schema assembly is consistent with this workspace.

I did not rerun the full cargo test suite; review verification was source/diff inspection, CodeGraph inspection of the retrieval/zone retrieval relationship, `git diff --check`, commit/file-scope inspection, and feature inspection for `serde_json`.

VERDICT: FIXES_REQUIRED
