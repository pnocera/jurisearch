//! `jurisearch-deploy` — the operator deployment layer (plan `01-makeitsimpletodeploy`, Phase 1).
//!
//! This crate owns:
//! - the strict [`SiteConfig`] parser (`deny_unknown_fields`, required fields enforced);
//! - deterministic [`RenderedSite`] env-file + systemd-unit rendering;
//! - the LOOPBACK-ONLY site query-embedder guard (site-config-scoped; NOT in `jurisearch-embed`);
//! - shared config primitives ([`secret`]) — secret redaction + secret-file permission helpers —
//!   designed for reuse by the later producer config parser (M2-B).
//!
//! The `jurisearchctl` binary (`src/bin/jurisearchctl.rs`) is the operator surface over these APIs.

pub mod bind;
pub mod config;
pub mod error;
pub mod render;
pub mod scaffold;
pub mod secret;
pub mod validate;

pub use config::{
    DatabaseConfig, EmbedderConfig, LicenseConfig, SITE_CONFIG_EXAMPLE, SiteConfig, SiteSection,
    SyncConfig, SystemConfig, TrustAnchorConfig, TrustConfig, TrustPurpose,
};
pub use error::{DeployError, Diagnostic, ValidationErrors};
pub use render::{RenderedFile, RenderedSite};
pub use secret::{SecretString, redact};

#[cfg(test)]
mod tests;
