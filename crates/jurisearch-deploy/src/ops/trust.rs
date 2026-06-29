//! `site bootstrap-trust` (plan `01` Phase 5): install the configured package/license trust anchors via
//! syncd, install a configured license token, with the NON-NEGOTIABLE rule that a trust anchor is NEVER
//! silently replaced.
//!
//! The storage primitive `install_trust_anchor` is an UPSERT (`ON CONFLICT … DO UPDATE SET
//! public_key_hex = EXCLUDED.public_key_hex`), so calling it blindly WOULD silently swap an anchor's key
//! material. This module therefore reads the installed anchors first and CLASSIFIES each configured
//! anchor as install / unchanged / conflicting-change — refusing the whole bootstrap if any anchor would
//! change. The classification is a pure function ([`plan_anchor_installs`]) so the "never silently
//! replaced" gate is unit-tested without a live DB. Key ROTATION (a new `[[trust.anchor]]` with a new
//! key_id/epoch) is `Install` and proceeds; replacing an EXISTING (key_id, epoch, purpose) with new
//! bytes is the explicit operator action this gate refuses.

use jurisearch_package::crypto::{KeyEpoch, KeyId, TrustAnchor};
use jurisearch_storage::backend::WriterConnection;
use jurisearch_storage::trust::{LICENSE_PURPOSE, PACKAGE_PURPOSE, load_trust_anchors};
use jurisearch_syncd::{install_trust_anchor, install_verified_license_token};

use crate::config::{SiteConfig, TrustAnchorConfig, TrustPurpose};
use crate::error::DeployError;

/// The identity of an installed/configured anchor for change detection: an anchor is "the same" anchor
/// when (key_id, key_epoch, purpose) match; its key MATERIAL is `public_key_hex`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorIdentity {
    pub key_id: String,
    pub key_epoch: u32,
    pub purpose: String,
    pub public_key_hex: String,
}

/// What `bootstrap-trust` would do to one configured anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorAction {
    /// No anchor with this (key_id, epoch, purpose) is installed → install it (incl. key rotation).
    Install(AnchorIdentity),
    /// An anchor with this identity AND the same key material is already installed → no-op (idempotent).
    Unchanged(AnchorIdentity),
    /// An anchor with this identity is installed with DIFFERENT key material → REFUSE (never silently
    /// replace a trust root; key rotation must add a NEW anchor, not overwrite an existing one).
    Conflict {
        identity: AnchorIdentity,
        installed_key: String,
    },
    /// TWO CONFIGURED anchors in this one `site.toml` share the same identity but carry DIFFERENT key
    /// material → REFUSE before any write (the second would silently overwrite the first via the
    /// upserting installer). A trust root is never silently chosen between two config rows.
    DuplicateConfig {
        identity: AnchorIdentity,
        first_key: String,
    },
}

/// The full plan over all configured anchors. A plan with ANY conflict is refused atomically (nothing is
/// installed) so a partial bootstrap can never leave a half-rotated trust store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorPlan {
    pub actions: Vec<AnchorAction>,
}

impl AnchorPlan {
    #[must_use]
    pub fn conflicts(&self) -> Vec<&AnchorAction> {
        self.actions
            .iter()
            .filter(|action| {
                matches!(
                    action,
                    AnchorAction::Conflict { .. } | AnchorAction::DuplicateConfig { .. }
                )
            })
            .collect()
    }

    #[must_use]
    pub fn has_conflict(&self) -> bool {
        !self.conflicts().is_empty()
    }

    /// The identities that will actually be installed (excludes unchanged + conflicting).
    #[must_use]
    pub fn to_install(&self) -> Vec<&AnchorIdentity> {
        self.actions
            .iter()
            .filter_map(|action| match action {
                AnchorAction::Install(identity) => Some(identity),
                _ => None,
            })
            .collect()
    }
}

/// PURE: classify each configured anchor against the installed set. The "never silently replaced"
/// invariant is exactly this function — a same-identity, different-key anchor is a `Conflict`.
#[must_use]
pub fn plan_anchor_installs(
    installed: &[AnchorIdentity],
    configured: &[(TrustPurpose, &TrustAnchorConfig)],
) -> AnchorPlan {
    let mut actions = Vec::new();
    // Identities already seen in THIS config, so two `[[trust.anchor]]` rows with the same
    // (key_id, epoch, purpose) cannot both reach syncd's upserting `install_trust_anchor` (the second
    // would silently overwrite the first). Detected BEFORE the installed-DB comparison, so a same-config
    // duplicate is refused even on an EMPTY DB.
    let mut seen: Vec<AnchorIdentity> = Vec::new();
    for (purpose, anchor) in configured {
        let identity = AnchorIdentity {
            key_id: anchor.key_id.clone(),
            key_epoch: anchor.key_epoch,
            purpose: purpose.as_str().to_owned(),
            public_key_hex: anchor.public_key_hex.clone(),
        };
        if let Some(first) = seen.iter().find(|prior| {
            prior.key_id == identity.key_id
                && prior.key_epoch == identity.key_epoch
                && prior.purpose == identity.purpose
        }) {
            if first
                .public_key_hex
                .eq_ignore_ascii_case(&identity.public_key_hex)
            {
                // Same identity + same key: a redundant duplicate. Collapse idempotently (the first
                // occurrence already drives the install) — emit nothing for the duplicate.
                continue;
            }
            // Same identity + DIFFERENT key within one config: refuse the whole bootstrap.
            actions.push(AnchorAction::DuplicateConfig {
                first_key: first.public_key_hex.clone(),
                identity,
            });
            continue;
        }
        seen.push(identity.clone());
        let action = match installed.iter().find(|existing| {
            existing.key_id == identity.key_id
                && existing.key_epoch == identity.key_epoch
                && existing.purpose == identity.purpose
        }) {
            None => AnchorAction::Install(identity),
            Some(existing)
                if existing
                    .public_key_hex
                    .eq_ignore_ascii_case(&identity.public_key_hex) =>
            {
                AnchorAction::Unchanged(identity)
            }
            Some(existing) => AnchorAction::Conflict {
                installed_key: existing.public_key_hex.clone(),
                identity,
            },
        };
        actions.push(action);
    }
    AnchorPlan { actions }
}

/// The configured anchors paired with their purpose token, in config order.
#[must_use]
pub fn configured_anchors(config: &SiteConfig) -> Vec<(TrustPurpose, &TrustAnchorConfig)> {
    config
        .trust
        .anchor
        .iter()
        .map(|anchor| (anchor.purpose, anchor))
        .collect()
}

/// The summary returned to the operator after a successful bootstrap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapOutcome {
    pub installed: usize,
    pub unchanged: usize,
    pub license_installed: bool,
}

/// Read the installed anchors (both purposes) and build the change plan against the configured anchors.
/// Live: opens the writer connection.
pub fn read_anchor_plan(
    conn: &dyn WriterConnection,
    config: &SiteConfig,
) -> Result<AnchorPlan, DeployError> {
    let mut installed = Vec::new();
    for purpose in [PACKAGE_PURPOSE, LICENSE_PURPOSE] {
        let mut client = conn
            .writer_client()
            .map_err(|error| storage_err("trust.read.connect", error))?;
        let anchors = load_trust_anchors(&mut client, purpose)
            .map_err(|error| storage_err("trust.read", error))?;
        for anchor in anchors {
            installed.push(AnchorIdentity {
                key_id: anchor.key_id.0,
                key_epoch: anchor.key_epoch.0,
                purpose: purpose.to_owned(),
                public_key_hex: anchor.public_key_hex,
            });
        }
    }
    Ok(plan_anchor_installs(
        &installed,
        &configured_anchors(config),
    ))
}

/// Install the configured trust anchors + license token (plan `01` Phase 5). REFUSES if any anchor would
/// silently change. Idempotent: unchanged anchors are skipped; re-running is a no-op.
pub fn bootstrap_trust(
    conn: &dyn WriterConnection,
    config: &SiteConfig,
) -> Result<BootstrapOutcome, DeployError> {
    let plan = read_anchor_plan(conn, config)?;
    if plan.has_conflict() {
        return Err(conflict_error(&plan));
    }
    let mut installed = 0;
    for action in &plan.actions {
        match action {
            AnchorAction::Install(identity) => {
                let anchor = TrustAnchor {
                    key_id: KeyId(identity.key_id.clone()),
                    key_epoch: KeyEpoch(identity.key_epoch),
                    algorithm: "ed25519".to_owned(),
                    public_key_hex: identity.public_key_hex.clone(),
                };
                install_trust_anchor(conn, &anchor, &identity.purpose)
                    .map_err(|error| sync_err("trust.install", error))?;
                installed += 1;
            }
            AnchorAction::Unchanged(_)
            | AnchorAction::Conflict { .. }
            | AnchorAction::DuplicateConfig { .. } => {}
        }
    }

    let unchanged = plan
        .actions
        .iter()
        .filter(|action| matches!(action, AnchorAction::Unchanged(_)))
        .count();

    let mut license_installed = false;
    if let Some(license) = &config.license {
        let token_json =
            std::fs::read_to_string(&license.token_json).map_err(|source| DeployError::Read {
                path: license.token_json.clone(),
                source,
            })?;
        install_verified_license_token(conn, &token_json)
            .map_err(|error| sync_err("license.install", error))?;
        license_installed = true;
    }

    Ok(BootstrapOutcome {
        installed,
        unchanged,
        license_installed,
    })
}

fn conflict_error(plan: &AnchorPlan) -> DeployError {
    let mut errors = crate::error::ValidationErrors::default();
    for action in plan.conflicts() {
        match action {
            AnchorAction::Conflict {
                identity,
                installed_key,
            } => errors.push(
                "trust.anchor.silent_replace_refused",
                format!(
                    "trust anchor key_id={} epoch={} purpose={} is already installed with a DIFFERENT \
                     public key (installed `{}`, config `{}`); a trust root is NEVER silently replaced",
                    identity.key_id,
                    identity.key_epoch,
                    identity.purpose,
                    redact_key(installed_key),
                    redact_key(&identity.public_key_hex),
                ),
                "key rotation must ADD a new [[trust.anchor]] with a new key_id/epoch — to deliberately \
                 replace this exact anchor, remove the installed row out-of-band first",
            ),
            AnchorAction::DuplicateConfig {
                identity,
                first_key,
            } => errors.push(
                "trust.anchor.duplicate_config",
                format!(
                    "site.toml configures trust anchor key_id={} epoch={} purpose={} TWICE with \
                     DIFFERENT public keys (`{}` vs `{}`); a trust root is never silently chosen between \
                     two config rows",
                    identity.key_id,
                    identity.key_epoch,
                    identity.purpose,
                    redact_key(first_key),
                    redact_key(&identity.public_key_hex),
                ),
                "remove the duplicate [[trust.anchor]] or fix its public_key_hex — key rotation must use \
                 a NEW key_id/epoch",
            ),
            AnchorAction::Install(_) | AnchorAction::Unchanged(_) => {}
        }
    }
    DeployError::Validation(errors)
}

/// Read the INSTALLED package + license anchor counts (for `site doctor`'s advisory trust diagnostics).
/// Live: opens the writer connection. Returns `(package_anchor_count, license_anchor_count)`.
pub fn installed_anchor_counts(conn: &dyn WriterConnection) -> Result<(usize, usize), DeployError> {
    let mut package_client = conn
        .writer_client()
        .map_err(|error| storage_err("trust.read.connect", error))?;
    let package = load_trust_anchors(&mut package_client, PACKAGE_PURPOSE)
        .map_err(|error| storage_err("trust.read", error))?
        .len();
    let mut license_client = conn
        .writer_client()
        .map_err(|error| storage_err("trust.read.connect", error))?;
    let license = load_trust_anchors(&mut license_client, LICENSE_PURPOSE)
        .map_err(|error| storage_err("trust.read", error))?
        .len();
    Ok((package, license))
}

/// Show only a short prefix of a public key in diagnostics (it is public, but a full 64-char dump is
/// noise; this keeps the message readable while still distinguishing two keys).
fn redact_key(hex: &str) -> String {
    let prefix: String = hex.chars().take(8).collect();
    format!("{prefix}…")
}

fn storage_err(
    code: &'static str,
    error: jurisearch_storage::runtime::StorageError,
) -> DeployError {
    let mut errors = crate::error::ValidationErrors::default();
    errors.push(
        code,
        error.to_string(),
        "check the [database] connection + writer role",
    );
    DeployError::Validation(errors)
}

fn sync_err(code: &'static str, error: jurisearch_syncd::SyncError) -> DeployError {
    let mut errors = crate::error::ValidationErrors::default();
    errors.push(
        code,
        error.to_string(),
        "check trust anchors + the license token input",
    );
    DeployError::Validation(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anchor(key_id: &str, epoch: u32, key: &str) -> TrustAnchorConfig {
        TrustAnchorConfig {
            purpose: TrustPurpose::Package,
            key_id: key_id.to_owned(),
            key_epoch: epoch,
            public_key_hex: key.to_owned(),
            algorithm: "ed25519".to_owned(),
        }
    }

    fn installed(key_id: &str, epoch: u32, purpose: &str, key: &str) -> AnchorIdentity {
        AnchorIdentity {
            key_id: key_id.to_owned(),
            key_epoch: epoch,
            purpose: purpose.to_owned(),
            public_key_hex: key.to_owned(),
        }
    }

    #[test]
    fn a_new_anchor_is_install() {
        let cfg = anchor("k1", 1, "aa");
        let plan = plan_anchor_installs(&[], &[(TrustPurpose::Package, &cfg)]);
        assert!(matches!(plan.actions[0], AnchorAction::Install(_)));
        assert!(!plan.has_conflict());
    }

    #[test]
    fn the_same_key_is_unchanged_idempotent() {
        let cfg = anchor("k1", 1, "AABB");
        let existing = installed("k1", 1, "package", "aabb"); // case-insensitive hex match
        let plan = plan_anchor_installs(&[existing], &[(TrustPurpose::Package, &cfg)]);
        assert!(matches!(plan.actions[0], AnchorAction::Unchanged(_)));
        assert!(!plan.has_conflict());
        assert!(plan.to_install().is_empty());
    }

    #[test]
    fn a_changed_key_for_the_same_identity_is_a_refused_conflict() {
        let cfg = anchor("k1", 1, "ffff");
        let existing = installed("k1", 1, "package", "aaaa");
        let plan = plan_anchor_installs(&[existing], &[(TrustPurpose::Package, &cfg)]);
        assert!(
            plan.has_conflict(),
            "a same-identity key change must be a conflict"
        );
        match &plan.actions[0] {
            AnchorAction::Conflict { installed_key, .. } => assert_eq!(installed_key, "aaaa"),
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn key_rotation_via_a_new_epoch_is_install_not_conflict() {
        // Same key_id, NEW epoch + new key material = a deliberate rotation → Install, never Conflict.
        let cfg = anchor("k1", 2, "ffff");
        let existing = installed("k1", 1, "package", "aaaa");
        let plan = plan_anchor_installs(&[existing], &[(TrustPurpose::Package, &cfg)]);
        assert!(matches!(plan.actions[0], AnchorAction::Install(_)));
        assert!(!plan.has_conflict());
    }

    #[test]
    fn two_configured_anchors_same_identity_different_keys_are_refused_on_empty_db() {
        // The BLOCKER case: two `[[trust.anchor]]` rows with the SAME (key_id, epoch, purpose) but
        // DIFFERENT keys, against an EMPTY DB, must be refused with NOTHING installed — proving a trust
        // anchor is never silently replaced even within one bootstrap.
        let first = anchor("k1", 1, "aaaa");
        let second = anchor("k1", 1, "ffff");
        let plan = plan_anchor_installs(
            &[],
            &[
                (TrustPurpose::Package, &first),
                (TrustPurpose::Package, &second),
            ],
        );
        // `has_conflict()` makes `bootstrap_trust` refuse the WHOLE plan before any write — atomic, so
        // even the first (otherwise-installable) anchor is never written.
        assert!(
            plan.has_conflict(),
            "a same-config identity+key change must conflict"
        );
        assert!(
            plan.actions
                .iter()
                .any(|a| matches!(a, AnchorAction::DuplicateConfig { .. }))
        );
        // The conflict surfaces a distinct, actionable diagnostic (no silent replace).
        let error = conflict_error(&plan).to_string();
        assert!(
            error.contains("trust.anchor.duplicate_config"),
            "got {error}"
        );
    }

    #[test]
    fn two_configured_anchors_same_identity_same_key_collapse_idempotently() {
        // Same identity + same key (case-insensitive): a redundant duplicate collapses to ONE install,
        // never a conflict.
        let first = anchor("k1", 1, "AABB");
        let second = anchor("k1", 1, "aabb");
        let plan = plan_anchor_installs(
            &[],
            &[
                (TrustPurpose::Package, &first),
                (TrustPurpose::Package, &second),
            ],
        );
        assert!(!plan.has_conflict());
        assert_eq!(plan.actions.len(), 1, "the duplicate must collapse");
        assert_eq!(plan.to_install().len(), 1);
    }

    #[test]
    fn the_same_key_id_under_a_different_purpose_is_not_a_conflict() {
        let mut cfg = anchor("k1", 1, "ffff");
        cfg.purpose = TrustPurpose::License;
        let existing = installed("k1", 1, "package", "aaaa"); // different purpose
        let plan = plan_anchor_installs(&[existing], &[(TrustPurpose::License, &cfg)]);
        assert!(matches!(plan.actions[0], AnchorAction::Install(_)));
        assert!(!plan.has_conflict());
    }
}
