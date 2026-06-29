//! Official-API enrichment helpers shared by the producer pipeline (`enrich_zones`) and the CLI read
//! path (`fetch --part --online`). Extracted from `jurisearch-cli` (work/10 M1-C):
//!
//! - `archive`: the shared `official_api_responses` archive (hash + persist a raw upstream exchange).
//! - `decision_part` + `judilibre_zones`: the `decision_zones` overlay (Judilibre zone enrichment).
//!
//! The legislation-citation collection path stays in `jurisearch-cli` (it is index-opening and not a
//! named producer seam); it consumes `archive::*` from here.

pub mod archive;
pub mod decision_part;
pub mod judilibre_zones;

pub use archive::*;
pub use decision_part::*;
pub use judilibre_zones::*;
