//! Official-API enrichment orchestration. The shared archive + `decision_zones` overlay
//! (`archive`/`decision_part`/`judilibre_zones`) moved to `jurisearch-pipeline` (work/10 M1-C) and is
//! re-exported here so the read path (`fetch --part --online`) and tests keep their `crate::<fn>`
//! references. The legislation-citation collection/resolution path stays in the CLI (it is
//! index-opening and not a named producer seam); it consumes the archive helpers from the pipeline.

pub(crate) mod legislation;

pub(crate) use jurisearch_pipeline::enrichment::*;
pub(crate) use legislation::*;
