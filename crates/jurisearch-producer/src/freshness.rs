//! Freshness policy + the DEFERRED Judilibre accelerator affordance (M7, resolved decision #6).
//!
//! v1 jurisprudence freshness is **daily DILA polling**: the `jurisearch-producer-jurisprudence.timer`
//! fetches and ingests new DILA archives once a day. A post-v1 "Judilibre freshness accelerator" — using
//! the PISTE/Judilibre API to surface same-day Cassation *Bulletin* decisions before the next DILA drop —
//! is explicitly **deferred and NOT implemented in this release**.
//!
//! This module is the single honest source of that fact: [`JudilibreAccelerator::status`] is a clear
//! "not implemented in this release" diagnostic any flag/command that references the accelerator can
//! surface, instead of pretending the feature exists.
//!
//! IMPORTANT — the core `update` path has NO hard dependency on Judilibre. Judilibre is used ONLY for
//! optional *zone enrichment* of cass/inca (`crate::update::enrich_group`), which HONESTLY SKIPS
//! (`EnrichmentMode::SkippedNoCredentials`, exit class `published-enrich-degraded`) when no PISTE
//! credentials are present — it never blocks ingest/embed/publish. So Judilibre API unavailability
//! degrades to DILA-only freshness and a still-successful (degraded) publish; it never blocks core updates.

use serde::Serialize;

/// The v1 freshness policy descriptor (what drives jurisprudence freshness today).
pub const V1_FRESHNESS_POLICY: &str = "daily-dila-polling";

/// The status of the (deferred) Judilibre freshness accelerator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JudilibreAccelerator {
    /// Stable machine-readable state; always `deferred-not-implemented` in this release.
    pub state: &'static str,
    /// The v1 freshness path that IS in effect.
    pub v1_freshness: &'static str,
    /// Whether the core `update` path depends on Judilibre being reachable (it does NOT).
    pub blocks_core_update: bool,
    /// A human-readable diagnostic for any flag/command that references the accelerator.
    pub message: &'static str,
}

impl JudilibreAccelerator {
    /// The honest deferral diagnostic. There is no accelerator to run in this release.
    #[must_use]
    pub fn status() -> Self {
        Self {
            state: "deferred-not-implemented",
            v1_freshness: V1_FRESHNESS_POLICY,
            blocks_core_update: false,
            message: "the Judilibre same-day freshness accelerator is deferred and not implemented in \
                      this release; v1 jurisprudence freshness is daily DILA polling. Judilibre is used \
                      only for optional cass/inca zone enrichment, which honestly skips without PISTE \
                      credentials and never blocks ingest/embed/publish.",
        }
    }
}

impl Default for JudilibreAccelerator {
    fn default() -> Self {
        Self::status()
    }
}

#[cfg(test)]
mod tests {
    use super::JudilibreAccelerator;

    #[test]
    fn judilibre_accelerator_is_deferred_and_never_blocks_core_update() {
        let status = JudilibreAccelerator::status();
        assert_eq!(status.state, "deferred-not-implemented");
        assert_eq!(status.v1_freshness, "daily-dila-polling");
        assert!(
            !status.blocks_core_update,
            "Judilibre unavailability must degrade to DILA-only, never block core updates"
        );
    }
}
