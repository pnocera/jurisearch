//! Package kinds and what forces each (design §6.1).

use serde::{Deserialize, Serialize};

/// The kind of package being applied (design §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageKind {
    /// First load of a corpus, shipped on physical media. Loads into a fresh per-corpus generation.
    Baseline,
    /// Breaking-change full reissue (re-embed / builder bump / corpus-rewriting migration), shipped
    /// on physical media. Same shape as a baseline but **scope-replaces only that corpus's** server
    /// set, preserving `jurisearch_app` and other corpora (§7.4).
    Rebaseline,
    /// Scheduled diff since the previous package, shipped over the network. Ordered, gap-free apply
    /// into the corpus's **active** generation (§7.3).
    Incremental,
}

impl PackageKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            PackageKind::Baseline => "baseline",
            PackageKind::Rebaseline => "rebaseline",
            PackageKind::Incremental => "incremental",
        }
    }

    /// Whether this kind loads into a **fresh, empty** generation (baseline/rebaseline) rather than
    /// the active one. Mirrors the embedded manifest's `requires_empty_generation` (§6.2.2).
    #[must_use]
    pub const fn requires_empty_generation(self) -> bool {
        matches!(self, PackageKind::Baseline | PackageKind::Rebaseline)
    }

    /// Whether this kind is delivered on physical media (baseline/rebaseline) rather than the
    /// network (incremental). Design §6.1 channel column.
    #[must_use]
    pub const fn is_media(self) -> bool {
        matches!(self, PackageKind::Baseline | PackageKind::Rebaseline)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_kind_wire_tokens() {
        assert_eq!(PackageKind::Baseline.as_str(), "baseline");
        assert_eq!(PackageKind::Rebaseline.as_str(), "rebaseline");
        assert_eq!(PackageKind::Incremental.as_str(), "incremental");
    }

    #[test]
    fn media_kinds_require_empty_generation() {
        assert!(PackageKind::Baseline.requires_empty_generation());
        assert!(PackageKind::Rebaseline.requires_empty_generation());
        assert!(!PackageKind::Incremental.requires_empty_generation());
        assert!(PackageKind::Baseline.is_media());
        assert!(!PackageKind::Incremental.is_media());
    }

    #[test]
    fn package_kind_round_trips() {
        for kind in [
            PackageKind::Baseline,
            PackageKind::Rebaseline,
            PackageKind::Incremental,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(serde_json::from_str::<PackageKind>(&json).unwrap(), kind);
        }
    }
}
