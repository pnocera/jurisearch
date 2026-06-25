//! Official-API enrichment orchestration (the decision-part / Judilibre-zone /
//! legislation-citation domain shared by the read path (`fetch --part --online`) and the
//! ingest path (`enrich-zones`, `collect/enrich-legislation-citations`)). Submodules are
//! re-exported so callers reach the helpers as `crate::<fn>`.
//!
//! - `archive`: the shared `official_api_responses` archive (used by legislation + zones).
//! - `decision_part` + `judilibre_zones`: share the `decision_zones` overlay.
//! - `legislation`: archived-visa collection + Legifrance resolution (no `decision_zones`).

pub(crate) mod archive;
pub(crate) mod decision_part;
pub(crate) mod judilibre_zones;
pub(crate) mod legislation;

pub(crate) use archive::*;
pub(crate) use decision_part::*;
pub(crate) use judilibre_zones::*;
pub(crate) use legislation::*;
