//! The pure citation-target parser moved to `jurisearch-query` (work/09 P4-4B) so the site `cite`
//! handler can build its response without a CLI adapter. Re-exported here so the CLI's existing
//! `crate::{parse_citation_target, ParsedCitationTarget, …}` references resolve unchanged.

pub(crate) use jurisearch_query::citation::*;
