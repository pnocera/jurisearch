//! The strict `site.toml` schema and its TOML loader.
//!
//! Strict = every struct uses `deny_unknown_fields`, so a typo or stray key is a hard parse error
//! rather than a silently ignored field. Required fields are enforced by `serde` (no `Option`).

use std::path::PathBuf;

use serde::Deserialize;

use crate::error::DeployError;

/// The operator-owned source of truth for one JuriSearch site (`/etc/jurisearch/site.toml`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteConfig {
    pub system: SystemConfig,
    pub site: SiteSection,
    pub database: DatabaseConfig,
    pub sync: SyncConfig,
    #[serde(default)]
    pub trust: TrustConfig,
    #[serde(default)]
    pub license: Option<LicenseConfig>,
    pub embedder: EmbedderConfig,
}

/// `[system]` — service identity and on-host directory layout.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemConfig {
    pub service_user: String,
    pub service_group: String,
    pub install_dir: PathBuf,
    pub config_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub state_dir: PathBuf,
}

/// `[site]` — the query-service bind + worker policy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteSection {
    /// `tcp://host:port` or `unix:///absolute/path`.
    pub bind: String,
    pub workers: u32,
    /// Required for any non-loopback TCP bind (the protocol has no client auth).
    #[serde(default)]
    pub allow_lan: bool,
    /// Additionally required for a wildcard (`0.0.0.0` / `::`) bind.
    #[serde(default)]
    pub allow_wildcard_lan: bool,
}

/// `[database]` — connection + role topology for the shared site PostgreSQL.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    pub host: String,
    pub port: u16,
    pub name: String,
    /// The bootstrap/admin identity used for provisioning (DB/roles/extensions/migrations).
    pub admin_user: String,
    /// The bootstrap database the admin connects to before the target DB necessarily exists.
    pub admin_database: String,
    /// Optional absolute path to a `0600` admin-password file (the password is never inline).
    #[serde(default)]
    pub admin_password_file: Option<PathBuf>,
    pub writer_user: String,
    pub read_user: String,
    pub owner_role: String,
    /// Test-only escape hatch allowing the three roles to be identical.
    #[serde(default)]
    pub unsafe_single_role: bool,
}

/// `[sync]` — the corpus catch-up source the syncd daemon consumes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncConfig {
    pub source_root: PathBuf,
    pub corpora: Vec<String>,
    pub interval_secs: u64,
}

/// `[trust]` — operator-installed verifying anchors (`[[trust.anchor]]`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustConfig {
    #[serde(default)]
    pub anchor: Vec<TrustAnchorConfig>,
}

/// One `[[trust.anchor]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustAnchorConfig {
    pub purpose: TrustPurpose,
    pub key_id: String,
    pub key_epoch: u32,
    pub public_key_hex: String,
    pub algorithm: String,
}

/// The two anchor purposes a site understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustPurpose {
    Package,
    License,
}

impl TrustPurpose {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TrustPurpose::Package => "package",
            TrustPurpose::License => "license",
        }
    }
}

/// `[license]` — an optional installed subscription token.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LicenseConfig {
    pub token_json: PathBuf,
}

/// `[embedder]` — the LOOPBACK-ONLY query embedding endpoint + the local llama-server it is served by.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbedderConfig {
    /// Reused from `jurisearch-embed`; site deployments must use `openai_compatible` (loopback).
    pub provider: jurisearch_embed::EmbeddingProvider,
    pub base_url: String,
    pub model_name: String,
    pub dimension: usize,
    pub normalize: bool,
    /// Fixed to `cls` in this phase (see validation).
    pub pooling: String,
    pub llama_server: PathBuf,
    pub model_path: PathBuf,
    pub tokenizer_json: PathBuf,
    pub port: u16,
}

impl EmbedderConfig {
    /// Build the shared `EmbeddingConfig` so storage-fingerprint logic is never duplicated here.
    #[must_use]
    pub fn to_embedding_config(&self) -> jurisearch_embed::EmbeddingConfig {
        let mut config = jurisearch_embed::EmbeddingConfig::openai_compatible(
            self.base_url.clone(),
            None,
            self.model_name.clone(),
            self.dimension,
            self.normalize,
            self.pooling.clone(),
        );
        config.tokenizer_path = Some(self.tokenizer_json.clone());
        config
    }
}

impl SiteConfig {
    /// Strict-parse a `site.toml` from its text. Unknown keys / missing required fields are errors.
    pub fn parse_str(toml_text: &str, source_path: &std::path::Path) -> Result<Self, DeployError> {
        toml::from_str(toml_text).map_err(|source| DeployError::Parse {
            path: source_path.to_path_buf(),
            source,
        })
    }

    /// Read + strict-parse a `site.toml` from disk (does NOT validate; call [`SiteConfig::validate`]).
    pub fn from_path(path: &std::path::Path) -> Result<Self, DeployError> {
        let text = std::fs::read_to_string(path).map_err(|source| DeployError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse_str(&text, path)
    }

    /// Read, strict-parse, AND validate a `site.toml` from disk.
    pub fn load(path: &std::path::Path) -> Result<Self, DeployError> {
        let config = Self::from_path(path)?;
        config.validate()?;
        Ok(config)
    }
}

/// A complete, commented example `site.toml`. It is the single source for `site config-example` and
/// for `site init`, and is asserted to round-trip through parse + validate in the test suite.
pub const SITE_CONFIG_EXAMPLE: &str = r#"# JuriSearch site deployment config (site.toml).
# Operator-owned source of truth. Generated files under <config_dir>/generated and the systemd units
# are DERIVED from this file by `jurisearchctl site render` and may be overwritten.

[system]
service_user = "jurisearch"
service_group = "jurisearch"
install_dir = "/usr/local/bin"
config_dir = "/etc/jurisearch"
runtime_dir = "/run/jurisearch"
state_dir = "/var/lib/jurisearch"

[site]
# tcp://host:port (a non-loopback host needs allow_lan) or unix:///absolute/path.
bind = "tcp://100.100.20.30:8099"
workers = 8
allow_lan = true
allow_wildcard_lan = false

[database]
host = "127.0.0.1"
port = 5432
name = "jurisearch"
admin_user = "postgres"
admin_database = "postgres"
# Optional. Absolute path to a 0600 admin-password file; the password is never inline in this TOML.
admin_password_file = "/etc/jurisearch/secrets/postgres-admin-password"
writer_user = "jurisearch_write"
read_user = "jurisearch_read"
owner_role = "jurisearch_owner"

[sync]
source_root = "/srv/jurisearch/packages"
corpora = ["core"]
interval_secs = 30

[[trust.anchor]]
purpose = "package"
key_id = "producer-k1"
key_epoch = 1
public_key_hex = "0000000000000000000000000000000000000000000000000000000000000000"
algorithm = "ed25519"

[[trust.anchor]]
purpose = "license"
key_id = "license-k1"
key_epoch = 1
public_key_hex = "1111111111111111111111111111111111111111111111111111111111111111"
algorithm = "ed25519"

[license]
token_json = "/etc/jurisearch/license-token.json"

[embedder]
# Site query embeddings are LOOPBACK-ONLY (confidentiality boundary); base_url must resolve to
# localhost / 127.0.0.0/8 / ::1. Pooling is fixed to "cls" in this phase.
provider = "openai_compatible"
base_url = "http://127.0.0.1:8081"
model_name = "bge-m3"
dimension = 1024
normalize = true
pooling = "cls"
llama_server = "/usr/local/bin/llama-server"
model_path = "/srv/jurisearch/models/bge-m3-Q8_0.gguf"
tokenizer_json = "/srv/jurisearch/models/bge-m3-tokenizer.json"
port = 8081
"#;
