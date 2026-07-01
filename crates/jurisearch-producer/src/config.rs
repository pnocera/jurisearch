//! The strict `producer.toml` schema, its loader, and the typed accessors that bridge it to the reused
//! crate APIs (storage connection/provision configs, the embedding config, the package signer).
//!
//! Strict = every struct uses `deny_unknown_fields`, so a typo or stray key is a hard parse error.
//! Secrets are NEVER inline: passwords and the signing seed are referenced as `0600` files, loaded
//! through the shared [`jurisearch_deploy::secret`] permission helpers (no parallel secret path).

use std::path::{Path, PathBuf};

use jurisearch_embed::{EmbeddingConfig, EmbeddingProvider};
use jurisearch_fetch::ArchiveSource;
use jurisearch_package::crypto::{Ed25519Signer, KeyEpoch, KeyId};
use jurisearch_storage::backend::{ConnectionConfig, RoleSpec, WriterHandle, WriterVisibility};
use jurisearch_storage::provision::ProvisionConfig;
use serde::Deserialize;

use crate::error::ProducerError;

/// The operator-owned source of truth for one update-server (`/etc/jurisearch/producer.toml`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerConfig {
    pub producer: ProducerSection,
    pub database: DatabaseConfig,
    pub fetch: FetchSection,
    /// `[[fetch_group]]` — SCHEDULING units (what a timer fetches+ingests), NOT package corpora.
    #[serde(default, rename = "fetch_group")]
    pub fetch_groups: Vec<FetchGroupConfig>,
    pub package: PackageConfig,
    pub enrichment: EnrichmentConfig,
    pub embedding: EmbeddingSection,
    #[serde(default)]
    pub baseline_refresh: BaselineRefreshConfig,
    /// `[install]` — systemd unit/timer rendering + install targets (M3). All-defaulted so an existing
    /// `producer.toml` keeps parsing; every path is absolute.
    #[serde(default)]
    pub install: InstallConfig,
    /// `[alert]` — the fail-closed alert-hook seam (M3). A config-pointable command run on failure
    /// classes; no provider is hardcoded. Off by default (no hook configured).
    #[serde(default)]
    pub alert: AlertConfig,
}

/// `[producer]` — the served root, the DILA mirror, and local orchestration state.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerSection {
    /// Served root: the published manifest + packages land here.
    pub corpora_dir: PathBuf,
    /// Downloaded DILA mirror (per-source subdirs).
    pub archives_dir: PathBuf,
    /// Small local orchestration state (cursors, checkpoints, locks).
    pub state_dir: PathBuf,
    /// Remote-manifest `publisher` field.
    pub publisher: String,
    /// Remote-manifest `environment` field (e.g. `production`).
    pub environment: String,
}

/// `[database]` — the EXTERNAL producer PostgreSQL (never a self-managed `ManagedPostgres`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseConfig {
    pub host: String,
    pub port: u16,
    pub name: String,
    /// Bootstrap/admin identity used for `provision-db` (DB/roles/extensions/migrations).
    pub admin_user: String,
    /// The maintenance database the admin connects to before the target DB exists.
    pub admin_database: String,
    /// Optional absolute path to a `0600` admin-password file (never inline).
    #[serde(default)]
    pub admin_password_file: Option<PathBuf>,
    /// The least-privilege writer identity used for all DB-mutating producer work.
    pub writer_user: String,
    #[serde(default)]
    pub writer_password_file: Option<PathBuf>,
    /// The read role + the namespace owner role — the activation visibility the writer stamps.
    pub read_user: String,
    /// Optional `0600` password file for the READ role (needed when the external PG uses password auth
    /// for the read identity, so `provision-db` sets it and its read postcondition probe can connect).
    #[serde(default)]
    pub read_password_file: Option<PathBuf>,
    pub owner_role: String,
}

/// `[fetch]` — the DILA remote-listing/download policy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchSection {
    pub base_url: String,
    pub user_agent: String,
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: u32,
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// v1: keep every accepted official archive on Storebox. Only `"all"` is accepted in v1.
    #[serde(default = "default_retain_deltas")]
    pub retain_deltas: String,
}

fn default_max_concurrency() -> u32 {
    2
}
fn default_timeout_secs() -> u64 {
    120
}
fn default_retain_deltas() -> String {
    "all".to_owned()
}

/// One `[[fetch_group]]`: a named scheduling unit over one or more DILA sources.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FetchGroupConfig {
    pub name: String,
    pub sources: Vec<String>,
    pub cadence: String,
    /// Optional systemd `OnCalendar=` override for this group's timer. When absent, a daily default is
    /// derived from the group name (legislation 22:30, jurisprudence 23:30, otherwise 21:30) so LEGI's
    /// ~20–22h drop is in before the run.
    #[serde(default)]
    pub on_calendar: Option<String>,
}

impl FetchGroupConfig {
    /// The `OnCalendar=` expression for this group's timer (the override, or a daily default by name).
    #[must_use]
    pub fn on_calendar(&self) -> String {
        if let Some(expr) = &self.on_calendar {
            return expr.clone();
        }
        match self.name.as_str() {
            "legislation" => "*-*-* 22:30:00".to_owned(),
            "jurisprudence" => "*-*-* 23:30:00".to_owned(),
            _ => "*-*-* 21:30:00".to_owned(),
        }
    }

    /// The group's cadence as whole seconds (the freshness budget `status` ages the last successful run
    /// against). Recognises `hourly`/`daily`/`weekly` (case-insensitive); anything else falls back to a
    /// daily budget so an unfamiliar cadence is never silently treated as "never stale".
    #[must_use]
    pub fn cadence_secs(&self) -> u64 {
        match self.cadence.trim().to_ascii_lowercase().as_str() {
            "hourly" => 3_600,
            "weekly" => 604_800,
            _ => 86_400, // daily (and the conservative default).
        }
    }
}

/// `[install]` — where `jurisearch-producer install` renders units/timers and the binary/config they
/// reference. Every path is ABSOLUTE (systemd does not expand `$HOME`/env in unit paths). All-defaulted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallConfig {
    /// Where rendered `.service`/`.timer` units are written (e.g. `/etc/systemd/system`).
    #[serde(default = "default_unit_dir")]
    pub unit_dir: PathBuf,
    /// Absolute path to the installed `jurisearch-producer` binary the service runs.
    #[serde(default = "default_binary_path")]
    pub binary_path: PathBuf,
    /// Absolute path to the `producer.toml` the service passes as `--config`.
    #[serde(default = "default_config_path")]
    pub config_path: PathBuf,
    /// The dedicated, least-privilege service user/group.
    #[serde(default = "default_service_user")]
    pub service_user: String,
    /// `EnvironmentFile=` for PISTE/OpenRouter creds (mode 0600), absolute.
    #[serde(default = "default_environment_file")]
    pub environment_file: PathBuf,
    /// `RandomizedDelaySec=` on the timers, so the daily runs do not all fire on the hour.
    #[serde(default = "default_randomized_delay_secs")]
    pub randomized_delay_secs: u64,
}

impl Default for InstallConfig {
    fn default() -> Self {
        Self {
            unit_dir: default_unit_dir(),
            binary_path: default_binary_path(),
            config_path: default_config_path(),
            service_user: default_service_user(),
            environment_file: default_environment_file(),
            randomized_delay_secs: default_randomized_delay_secs(),
        }
    }
}

fn default_unit_dir() -> PathBuf {
    PathBuf::from("/etc/systemd/system")
}
fn default_binary_path() -> PathBuf {
    PathBuf::from("/usr/local/bin/jurisearch-producer")
}
fn default_config_path() -> PathBuf {
    PathBuf::from("/etc/jurisearch/producer.toml")
}
fn default_service_user() -> String {
    "jurisearch".to_owned()
}
fn default_environment_file() -> PathBuf {
    PathBuf::from("/etc/jurisearch/producer.env")
}
fn default_randomized_delay_secs() -> u64 {
    1800
}

/// `[alert]` — the fail-closed alert-hook seam. `hook_command` is run (argv-split, no shell) on a run
/// whose exit class is in `on_classes`, with the class/group/message passed as environment variables.
/// No provider is hardcoded; an empty `hook_command` disables alerting.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertConfig {
    /// Argv of the hook command (first element is the program). Empty ⇒ no alerting.
    #[serde(default)]
    pub hook_command: Vec<String>,
    /// Exit classes that trigger the hook. Empty ⇒ the default failure set.
    #[serde(default)]
    pub on_classes: Vec<String>,
}

/// `[package]` — single-corpus packaging + signing identity.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageConfig {
    /// v1: ALL ingested sources attribute to the single `core` corpus.
    #[serde(default = "default_corpus")]
    pub corpus: String,
    pub signing_key_id: String,
    #[serde(default = "default_key_epoch")]
    pub signing_key_epoch: u32,
    /// Absolute path to a `0600` file holding the 32-byte ed25519 seed as 64 hex chars.
    pub signing_key_seed_file: PathBuf,
    #[serde(default = "default_uri_base")]
    pub uri_base: String,
    #[serde(default = "default_max_retained")]
    pub max_retained_incrementals: usize,
}

fn default_corpus() -> String {
    "core".to_owned()
}
fn default_key_epoch() -> u32 {
    1
}
fn default_uri_base() -> String {
    "media://".to_owned()
}
fn default_max_retained() -> usize {
    200
}

/// `[enrichment]` — proactive Judilibre zone backfill policy.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnrichmentConfig {
    pub mode: EnrichmentModeConfig,
    /// Decision-date cutoff (`YYYY-MM-DD`) for Judilibre zone enrichment: only decisions dated on/after
    /// this date are enriched. Official zones exist only for recent (~2016+) decisions, so older ones
    /// have no zone coverage and enriching them only burns API quota. Omitting the key uses the default
    /// `2016-01-01`. TOML has no `null` literal, so the cutoff is always applied; to enrich older
    /// decisions set an earlier date (e.g. `1900-01-01`) rather than disabling it.
    #[serde(default = "default_judilibre_min_decision_date")]
    pub min_decision_date: Option<String>,
}

/// Producer default Judilibre enrichment cutoff: attempt only decisions dated `2016-01-01` or later.
fn default_judilibre_min_decision_date() -> Option<String> {
    Some("2016-01-01".to_owned())
}

/// True iff `value` is exactly an ISO calendar date `YYYY-MM-DD` with a real month/day (so a malformed
/// or nonexistent date like `2016-13-99` or `2016/01/01` is rejected here, not by a Postgres cast).
fn is_iso_calendar_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return false;
    }
    let (Some(year), Some(month), Some(day)) = (
        value.get(0..4).and_then(|s| s.parse::<u32>().ok()),
        value.get(5..7).and_then(|s| s.parse::<u32>().ok()),
        value.get(8..10).and_then(|s| s.parse::<u32>().ok()),
    ) else {
        return false;
    };
    // `parse::<u32>` accepts a leading `+`/whitespace on some inputs; the fixed-width slices above are
    // digits-only by construction (any non-digit fails the earlier length/separator check paths only for
    // separators, so re-verify each field is pure ASCII digits).
    if !value[0..4].bytes().all(|b| b.is_ascii_digit())
        || !value[5..7].bytes().all(|b| b.is_ascii_digit())
        || !value[8..10].bytes().all(|b| b.is_ascii_digit())
    {
        return false;
    }
    if year < 1 || !(1..=12).contains(&month) || day < 1 {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    day <= max_day
}

/// `auto` runs when PISTE creds are present (else honestly `SkippedNoCredentials`); `disabled` is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrichmentModeConfig {
    Auto,
    Disabled,
}

/// `[embedding]` — producer-side DOCUMENT embedding over public legal text. The STORAGE-fingerprint
/// fields (`model_name`/`dimension`/`normalize`) are kept SEPARATE from the provider request fields
/// (`request_model`/`base_url`), so an OpenRouter request id never changes the stored fingerprint.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingSection {
    /// Producer permits an external `openai_compatible` provider (public-text document embedding).
    pub provider: EmbeddingProvider,
    pub base_url: String,
    /// Canonical STORAGE-fingerprint model name (must match the site-local query embedder exactly).
    pub model_name: String,
    /// Provider request model (e.g. OpenRouter `baai/bge-m3`). NOT part of the storage fingerprint.
    #[serde(default)]
    pub request_model: Option<String>,
    pub dimension: usize,
    pub normalize: bool,
    pub pooling: String,
    /// Name of the env var holding the provider API key (the key itself is never inline).
    #[serde(default)]
    pub api_key_env: Option<String>,
}

/// `[baseline_refresh]` — DILA baseline re-issue policy. M2-B only records it; M3 acts on it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineRefreshConfig {
    pub mode: String,
}

impl Default for BaselineRefreshConfig {
    fn default() -> Self {
        Self {
            mode: "auto-on-new-baseline".to_owned(),
        }
    }
}

impl ProducerConfig {
    /// Strict-parse a `producer.toml` from its text. Unknown keys / missing required fields are errors.
    pub fn parse_str(toml_text: &str, source_path: &Path) -> Result<Self, ProducerError> {
        toml::from_str(toml_text).map_err(|source| ProducerError::ConfigParse {
            path: source_path.to_path_buf(),
            source,
        })
    }

    /// Read + strict-parse a `producer.toml` from disk (does NOT validate).
    pub fn from_path(path: &Path) -> Result<Self, ProducerError> {
        let text = std::fs::read_to_string(path).map_err(|source| ProducerError::ConfigRead {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse_str(&text, path)
    }

    /// Read, strict-parse, AND validate a `producer.toml` from disk.
    pub fn load(path: &Path) -> Result<Self, ProducerError> {
        let config = Self::from_path(path)?;
        config.validate()?;
        Ok(config)
    }

    /// Semantic validation beyond the TOML schema.
    pub fn validate(&self) -> Result<(), ProducerError> {
        if self.package.corpus.trim().is_empty() {
            return Err(ProducerError::ConfigInvalid(
                "[package].corpus must not be empty".to_owned(),
            ));
        }
        // v1 invariant: LEGI/CASS/CAPP/INCA/JADE are ONE `core` package corpus; the only legitimate
        // value in v1 is `core` (restricted add-ons get their own corpus, not folded in here).
        if self.package.corpus != "core" {
            return Err(ProducerError::ConfigInvalid(format!(
                "[package].corpus = `{}`; v1 packages all DILA sources as the single `core` corpus",
                self.package.corpus
            )));
        }
        if self.fetch.retain_deltas != "all" {
            return Err(ProducerError::ConfigInvalid(format!(
                "[fetch].retain_deltas = `{}`; v1 retains all accepted archives (\"all\")",
                self.fetch.retain_deltas
            )));
        }
        if self.fetch_groups.is_empty() {
            return Err(ProducerError::ConfigInvalid(
                "at least one [[fetch_group]] is required".to_owned(),
            ));
        }
        for group in &self.fetch_groups {
            if group.sources.is_empty() {
                return Err(ProducerError::ConfigInvalid(format!(
                    "fetch group `{}` lists no sources",
                    group.name
                )));
            }
            for token in &group.sources {
                ArchiveSource::from_token(token)
                    .ok_or_else(|| ProducerError::UnknownSource(token.clone()))?;
            }
        }
        if self.embedding.dimension == 0 {
            return Err(ProducerError::ConfigInvalid(
                "[embedding].dimension must be > 0".to_owned(),
            ));
        }
        // Operator config must be validated up front — do NOT defer to a runtime PostgreSQL `::date`
        // cast error mid-enrichment. Require an exact ISO calendar date (`YYYY-MM-DD`).
        if let Some(cutoff) = self.enrichment.min_decision_date.as_deref() {
            if !is_iso_calendar_date(cutoff) {
                return Err(ProducerError::ConfigInvalid(format!(
                    "[enrichment].min_decision_date = `{cutoff}` must be an ISO calendar date \
                     (YYYY-MM-DD)"
                )));
            }
        }
        // Every path RENDERED INTO a systemd unit (and the producer data/state paths referenced by
        // `ExecStart`/`ReadWritePaths`) MUST be ABSOLUTE: systemd does not expand `$HOME`/env in unit
        // file paths, so a relative value would render a unit that silently writes to the wrong place.
        // Reject relative values up front, BEFORE any unit is rendered.
        for (label, path) in [
            ("[install].unit_dir", self.install.unit_dir.as_path()),
            ("[install].binary_path", self.install.binary_path.as_path()),
            ("[install].config_path", self.install.config_path.as_path()),
            (
                "[install].environment_file",
                self.install.environment_file.as_path(),
            ),
            (
                "[producer].corpora_dir",
                self.producer.corpora_dir.as_path(),
            ),
            (
                "[producer].archives_dir",
                self.producer.archives_dir.as_path(),
            ),
            ("[producer].state_dir", self.producer.state_dir.as_path()),
        ] {
            if !path.is_absolute() {
                return Err(ProducerError::ConfigInvalid(format!(
                    "{label} = `{}` must be an ABSOLUTE path (it is rendered into a systemd unit; \
                     systemd does not expand env in unit file paths)",
                    path.display()
                )));
            }
        }
        // Reject world/group-readable secret files up front (defense in depth before they are trusted).
        for path in [
            self.database.admin_password_file.as_deref(),
            self.database.writer_password_file.as_deref(),
            self.database.read_password_file.as_deref(),
            Some(self.package.signing_key_seed_file.as_path()),
        ]
        .into_iter()
        .flatten()
        {
            check_secret_file_perms(path)?;
        }
        Ok(())
    }

    /// Resolve a DILA source token (e.g. `cass`) to the name of the fetch group that schedules it. The
    /// operator `rebaseline --source <src>` repair targets a SOURCE, but a rebaseline re-anchors the whole
    /// `core` corpus and the run is locked + ingested PER GROUP, so the repair runs over the source's group.
    pub fn group_for_source(&self, token: &str) -> Result<String, ProducerError> {
        // Reject an unknown token up front with the same diagnostic as the other source-taking commands.
        if ArchiveSource::from_token(token).is_none() {
            return Err(ProducerError::UnknownSource(token.to_owned()));
        }
        self.fetch_groups
            .iter()
            .find(|g| g.sources.iter().any(|s| s == token))
            .map(|g| g.name.clone())
            .ok_or_else(|| {
                ProducerError::ConfigInvalid(format!(
                    "source `{token}` is not listed in any [[fetch_group]]"
                ))
            })
    }

    /// Resolve a named fetch group to its ordered DILA sources.
    pub fn resolve_group(&self, name: &str) -> Result<Vec<ArchiveSource>, ProducerError> {
        let group = self
            .fetch_groups
            .iter()
            .find(|g| g.name == name)
            .ok_or_else(|| ProducerError::UnknownGroup(name.to_owned()))?;
        group
            .sources
            .iter()
            .map(|token| {
                ArchiveSource::from_token(token)
                    .ok_or_else(|| ProducerError::UnknownSource(token.clone()))
            })
            .collect()
    }

    /// Build the producer-side document [`EmbeddingConfig`]. The STORAGE-fingerprint fields come from
    /// `[embedding].model_name`/`dimension`/`normalize`/`pooling`; the provider `request_model` is set
    /// SEPARATELY and is excluded from `storage_embedding_fingerprint()`.
    #[must_use]
    pub fn embedding_config(&self) -> EmbeddingConfig {
        let api_key = self
            .embedding
            .api_key_env
            .as_deref()
            .and_then(|name| std::env::var(name).ok())
            .filter(|value| !value.trim().is_empty());
        let mut config = EmbeddingConfig::openai_compatible(
            self.embedding.base_url.clone(),
            api_key,
            self.embedding.model_name.clone(),
            self.embedding.dimension,
            self.embedding.normalize,
            self.embedding.pooling.clone(),
        );
        config.provider = self.embedding.provider;
        config.request_model = self
            .embedding
            .request_model
            .as_ref()
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty());
        // Producer document embeddings over public text are authoritative (not provisional).
        config.provisional = false;
        config
    }

    /// The producer's storage-embedding fingerprint (`model:dimension:normalize:<bool>`).
    #[must_use]
    pub fn storage_embedding_fingerprint(&self) -> String {
        self.embedding_config().storage_embedding_fingerprint()
    }

    /// The admin/maintenance [`ConnectionConfig`] used by `provision-db`.
    pub fn admin_connection(&self) -> Result<ConnectionConfig, ProducerError> {
        Ok(ConnectionConfig {
            host: self.database.host.clone(),
            port: self.database.port,
            dbname: self.database.admin_database.clone(),
            user: self.database.admin_user.clone(),
            password: self.load_optional_secret(self.database.admin_password_file.as_deref())?,
            application_name: "jurisearch-producer-provision".to_owned(),
        })
    }

    /// The typed [`ProvisionConfig`] for the external producer database (producer role profile).
    pub fn provision_config(&self) -> Result<ProvisionConfig, ProducerError> {
        Ok(ProvisionConfig {
            admin: self.admin_connection()?,
            target_db: self.database.name.clone(),
            roles: RoleSpec {
                read_role: self.database.read_user.clone(),
                writer_role: self.database.writer_user.clone(),
                owner_role: self.database.owner_role.clone(),
                // Load the configured password files so `provision-db` actually sets the role passwords
                // (`ALTER ROLE ... PASSWORD`) AND its writer/read postcondition probes can connect on a
                // password-auth external PG (WARN: previously `None`, so the writer probe could fail).
                read_password: self
                    .load_optional_secret(self.database.read_password_file.as_deref())?,
                writer_password: self
                    .load_optional_secret(self.database.writer_password_file.as_deref())?,
            },
        })
    }

    /// The external producer [`WriterHandle`] (a `DbClientSource`) used for all DB-mutating producer
    /// work. NEVER a `ManagedPostgres` — this is the external-PG seam.
    pub fn writer_handle(&self) -> Result<WriterHandle, ProducerError> {
        let writer = ConnectionConfig {
            host: self.database.host.clone(),
            port: self.database.port,
            dbname: self.database.name.clone(),
            user: self.database.writer_user.clone(),
            password: self.load_optional_secret(self.database.writer_password_file.as_deref())?,
            application_name: "jurisearch-producer".to_owned(),
        };
        let visibility = WriterVisibility {
            read_role: self.database.read_user.clone(),
            view_owner_role: self.database.owner_role.clone(),
        };
        Ok(WriterHandle::new(writer, visibility))
    }

    /// Load the producer package signer from the `0600` seed file (64 hex chars → 32-byte ed25519 seed).
    pub fn signer(&self) -> Result<Ed25519Signer, ProducerError> {
        let path = &self.package.signing_key_seed_file;
        check_secret_file_perms(path)?;
        let raw = std::fs::read_to_string(path).map_err(|source| ProducerError::Secret {
            path: path.clone(),
            message: format!("read failed: {source}"),
        })?;
        let bytes = hex::decode(raw.trim()).map_err(|err| ProducerError::Secret {
            path: path.clone(),
            message: format!("expected 64 hex chars (a 32-byte ed25519 seed): {err}"),
        })?;
        let seed: [u8; 32] = bytes.try_into().map_err(|_| ProducerError::Secret {
            path: path.clone(),
            message: "signing seed must decode to exactly 32 bytes".to_owned(),
        })?;
        Ok(Ed25519Signer::from_seed(
            &seed,
            KeyId(self.package.signing_key_id.clone()),
            KeyEpoch(self.package.signing_key_epoch),
        ))
    }

    fn load_optional_secret(&self, path: Option<&Path>) -> Result<Option<String>, ProducerError> {
        match path {
            None => Ok(None),
            Some(path) => {
                check_secret_file_perms(path)?;
                let value =
                    std::fs::read_to_string(path).map_err(|source| ProducerError::Secret {
                        path: path.to_path_buf(),
                        message: format!("read failed: {source}"),
                    })?;
                Ok(Some(value.trim().to_owned()))
            }
        }
    }
}

/// Reject a secret file that is missing or world/group-readable (reuses the deploy permission helper).
fn check_secret_file_perms(path: &Path) -> Result<(), ProducerError> {
    match jurisearch_deploy::secret::is_world_or_group_accessible(path) {
        Ok(true) => Err(ProducerError::Secret {
            path: path.to_path_buf(),
            message: "is group/world accessible; tighten to mode 0600".to_owned(),
        }),
        Ok(false) => Ok(()),
        Err(source) => Err(ProducerError::Secret {
            path: path.to_path_buf(),
            message: format!("cannot stat: {source}"),
        }),
    }
}

/// A complete, commented example `producer.toml`. Single source for `config-example`; asserted to
/// round-trip through parse + validate in the test suite.
pub const PRODUCER_CONFIG_EXAMPLE: &str = r#"# JuriSearch update-server (producer) config (producer.toml).
# Operator-owned source of truth for the package-origin orchestrator. Secrets are NEVER inline; they
# are referenced as 0600 files / env vars.

[producer]
corpora_dir = "/srv/jurisearch/storebox/packages"   # served root (manifest + packages)
archives_dir = "/srv/jurisearch/storebox/archives"   # downloaded DILA mirror (per-source subdirs)
state_dir = "/var/lib/jurisearch-producer"           # local orchestration state (cursors/checkpoints)
publisher = "jurisearch"
environment = "production"

[database]
# EXTERNAL producer PostgreSQL (never a self-managed instance). On bear this points CT 111 -> CT 110.
host = "192.168.0.110"
port = 5432
name = "jurisearch"
admin_user = "postgres"
admin_database = "postgres"
admin_password_file = "/etc/jurisearch/secrets/postgres-admin-password"
writer_user = "jurisearch_write"
writer_password_file = "/etc/jurisearch/secrets/jurisearch-write-password"
read_user = "jurisearch_read"
# Set when the external PG uses password auth for the read role (provision-db then sets it + probes it):
# read_password_file = "/etc/jurisearch/secrets/jurisearch-read-password"
owner_role = "jurisearch_owner"

[fetch]
base_url = "https://echanges.dila.gouv.fr/OPENDATA"
user_agent = "jurisearch-producer/0.1 (+contact)"
max_concurrency = 2
timeout_secs = 120
retain_deltas = "all"

# Fetch groups = SCHEDULING units (what a timer fetches+ingests), NOT package corpora.
[[fetch_group]]
name = "legislation"
sources = ["legi"]
cadence = "daily"

[[fetch_group]]
name = "jurisprudence"
sources = ["cass", "inca", "capp", "jade"]
cadence = "daily"

[package]
# v1: ALL ingested sources attribute to the single `core` corpus. One producer_cycle, one manifest.
corpus = "core"
signing_key_id = "producer-k1"
signing_key_epoch = 1
signing_key_seed_file = "/etc/jurisearch/secrets/producer-signing.seed"
uri_base = "media://"
max_retained_incrementals = 200

[enrichment]
# auto = run Judilibre zone backfill when PISTE creds are present, else SkippedNoCredentials (honest).
# PISTE creds come from the environment, never inline:
#   JURISEARCH_PISTE_ENV, JURISEARCH_PISTE_JUDILIBRE_KEY_ID,
#   JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID / _SECRET
mode = "auto"
# min_decision_date = cutoff (YYYY-MM-DD): only decisions dated on/after this are enriched. Official
# Judilibre zones exist only for recent (~2016+) decisions, so older ones have no zone coverage and
# enriching them only burns API quota. Omit this key to use the default 2016-01-01 cutoff. TOML has no
# null literal, so the cutoff is always applied; to enrich older decisions set an earlier date (e.g.
# 1900-01-01) rather than disabling it.
min_decision_date = "2016-01-01"

[embedding]
# Producer-side DOCUMENT embedding over PUBLIC legal text; v1 may use a fast external OpenAI-compatible
# provider such as OpenRouter. The storage fingerprint (model_name/dimension/normalize) must match the
# site-local query embedder; request_model is the provider-specific id and never changes the fingerprint.
provider = "openai_compatible"
base_url = "https://openrouter.ai/api/v1"
model_name = "bge-m3"
request_model = "baai/bge-m3"
dimension = 1024
normalize = true
pooling = "cls"
api_key_env = "OPENROUTER_API_KEY"

[baseline_refresh]
# v1 default: adopt a newer DILA global baseline automatically via a recorded rebaseline run (M3).
mode = "auto-on-new-baseline"

[install]
# Where `jurisearch-producer install` renders the .service/.timer units (absolute paths only — systemd
# does not expand env in unit file paths). All fields are optional and default to these values.
unit_dir = "/etc/systemd/system"
binary_path = "/usr/local/bin/jurisearch-producer"
config_path = "/etc/jurisearch/producer.toml"
service_user = "jurisearch"
environment_file = "/etc/jurisearch/producer.env"   # 0600; PISTE + OPENROUTER_API_KEY creds
randomized_delay_secs = 1800                          # spread the daily runs off the hour

[alert]
# Fail-closed alert hook: an argv (no shell) run when a run's exit class is in `on_classes`. The class,
# group, message, and run id are passed as JURISEARCH_ALERT_* environment variables. No provider is
# hardcoded; leave `hook_command` empty to disable alerting.
hook_command = []                                     # e.g. ["/usr/local/bin/notify", "--producer"]
on_classes = []                                       # empty = the default failure set
"#;
