//! Case-insensitive ASCII substring search helpers moved to `jurisearch-query` (work/09 P4-4B, where
//! `legi_citation_routing` now lives); re-exported so the CLI's enrichment heuristics and tests keep
//! their `crate::{find_ascii_ci, rfind_ascii_ci}` references.

pub(crate) use jurisearch_query::rfind_ascii_ci;
// `find_ascii_ci` is exercised by the CLI ascii unit tests (via `crate::*`); the routing path that used
// it directly now lives in `jurisearch-query`, so it is test-only on the CLI side.
#[cfg_attr(not(test), allow(unused_imports))]
pub(crate) use jurisearch_query::find_ascii_ci;
