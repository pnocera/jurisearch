//! Case-insensitive ASCII substring search helpers moved to `jurisearch-query` (work/09 P4-4B, where
//! `legi_citation_routing` now lives); re-exported so the CLI's enrichment heuristics and tests keep
//! their `crate::{find_ascii_ci, rfind_ascii_ci}` references.

// Both helpers are exercised by the CLI ascii unit tests (via `crate::*`); the enrichment heuristic
// path that used `rfind_ascii_ci` directly now lives in `jurisearch-pipeline`, so on the CLI side these
// are test-only re-exports.
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use jurisearch_query::{find_ascii_ci, rfind_ascii_ci};
