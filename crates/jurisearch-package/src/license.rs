//! The client-installed entitlement assertion (plan P6, §11.3).
//!
//! A [`LicenseToken`] is a producer/licensing-signed claim that a client is entitled to a corpus at a
//! tier. It is wrapped in [`crate::signed::Signed`] for transport/storage and **re-verified against a
//! license-purpose trust anchor on every use** (the stored copy is local mutable state). The apply
//! path refuses a `Subscription`-tier package whose corpus/tier is not covered by a valid installed
//! token (`MissingEntitlement`, §6.3) — entitlement is an apply precondition, not URL hiding.

use crate::corpus::Corpus;
use serde::{Deserialize, Serialize};

/// The tier tokens a package marks itself OPEN with (no entitlement required). Producer baselines emit
/// `"all"`; both spellings mean "no subscription required" for the P6 gate.
pub const OPEN_TIERS: &[&str] = &["all", "open"];

/// Whether a package `tier` is open (needs no entitlement token).
#[must_use]
pub fn tier_is_open(tier: &str) -> bool {
    OPEN_TIERS.contains(&tier)
}

/// A signed entitlement assertion installed on the client (plan P6, §11.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseToken {
    pub entitlement_corpus: Corpus,
    pub tier: String,
    pub license_epoch: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<String>,
    /// RFC3339 UTC expiry (canonical `Z`); `None` = no expiry. Compared by the consumer with the DB
    /// clock (a SQL `not_after::timestamptz > now()` check), never by string-ordering arbitrary input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<String>,
}

impl LicenseToken {
    /// Whether this token covers a package's `(corpus, tier)` at `required_epoch` (EXACT tier match,
    /// per P6 — no implicit hierarchy). `audience` and `not_after` are enforced by the consumer's
    /// installed-token query (audience policy + DB-clock expiry).
    #[must_use]
    pub fn covers(&self, corpus: &str, tier: &str, required_epoch: u32) -> bool {
        self.entitlement_corpus.as_str() == corpus
            && self.tier == tier
            && self.license_epoch >= required_epoch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_tiers_need_no_token() {
        assert!(tier_is_open("all"));
        assert!(tier_is_open("open"));
        assert!(!tier_is_open("restricted"));
    }

    #[test]
    fn coverage_is_exact_corpus_and_tier_with_epoch_floor() {
        let token = LicenseToken {
            entitlement_corpus: Corpus::new("inpi").unwrap(),
            tier: "restricted".to_owned(),
            license_epoch: 3,
            audience: None,
            not_after: None,
        };
        assert!(token.covers("inpi", "restricted", 3));
        assert!(
            token.covers("inpi", "restricted", 2),
            "a newer token covers an older epoch"
        );
        assert!(
            !token.covers("inpi", "restricted", 4),
            "an older token does NOT cover a newer epoch"
        );
        assert!(!token.covers("core", "restricted", 3), "wrong corpus");
        assert!(
            !token.covers("inpi", "premium", 3),
            "wrong tier (no hierarchy)"
        );
    }
}
