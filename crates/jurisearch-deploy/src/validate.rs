//! Strict structural + policy validation of a parsed [`SiteConfig`].
//!
//! All diagnostics are collected and returned together. The loopback-only query-embedder guard lives
//! HERE (site-config-scoped), never in `jurisearch-embed` — the producer path legitimately uses
//! external embedding providers.

use std::path::Path;

use jurisearch_embed::{BaseUrlClass, EmbeddingProvider, base_url_class};

use crate::bind::{BindAddress, TcpExposure, parse_bind};
use crate::config::{SiteConfig, TrustPurpose};
use crate::error::ValidationErrors;
use crate::secret;

/// The single phase-1 supported pooling mode.
const REQUIRED_POOLING: &str = "cls";

impl SiteConfig {
    /// Validate the parsed config. Returns every problem at once.
    pub fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::default();
        self.validate_system(&mut errors);
        self.validate_site(&mut errors);
        self.validate_database(&mut errors);
        self.validate_sync(&mut errors);
        self.validate_trust(&mut errors);
        self.validate_embedder(&mut errors);
        self.validate_render_safety(&mut errors);
        errors.into_result()
    }

    fn validate_system(&self, errors: &mut ValidationErrors) {
        let system = &self.system;
        for (label, path) in [
            ("system.install_dir", system.install_dir.as_path()),
            ("system.config_dir", system.config_dir.as_path()),
            ("system.runtime_dir", system.runtime_dir.as_path()),
            ("system.state_dir", system.state_dir.as_path()),
        ] {
            require_absolute(errors, "system.path.relative", label, path);
        }
        if system.service_user.trim().is_empty() {
            errors.push(
                "system.user.empty",
                "system.service_user must not be empty",
                "set system.service_user (e.g. \"jurisearch\")",
            );
        }
        if system.service_group.trim().is_empty() {
            errors.push(
                "system.group.empty",
                "system.service_group must not be empty",
                "set system.service_group (e.g. \"jurisearch\")",
            );
        }
    }

    fn validate_site(&self, errors: &mut ValidationErrors) {
        let site = &self.site;
        if site.workers == 0 {
            errors.push(
                "site.workers.zero",
                "site.workers must be >= 1",
                "set site.workers to the desired bounded connection count (e.g. 8)",
            );
        }
        match parse_bind(&site.bind) {
            Err(error) => errors.push(
                "site.bind.malformed",
                format!("site.bind `{}` is invalid: {error}", site.bind),
                "use `tcp://host:port` or `unix:///absolute/path`",
            ),
            Ok(BindAddress::Unix { path }) => {
                if !Path::new(&path).is_absolute() {
                    errors.push(
                        "site.bind.unix.relative",
                        format!("site.bind unix socket path `{path}` must be absolute"),
                        "use `unix:///run/jurisearch/jurisearch-site.sock`",
                    );
                }
            }
            Ok(BindAddress::Tcp { exposure, .. }) => {
                self.validate_tcp_exposure(errors, exposure);
            }
        }
    }

    fn validate_tcp_exposure(&self, errors: &mut ValidationErrors, exposure: TcpExposure) {
        let site = &self.site;
        match exposure {
            TcpExposure::Loopback => {}
            TcpExposure::TrustedLan => {
                if !site.allow_lan {
                    errors.push(
                        "site.bind.lan.not_allowed",
                        "site.bind is a non-loopback (LAN) TCP address but site.allow_lan is false",
                        "set site.allow_lan = true (the service has NO client auth; trusted LAN only)",
                    );
                }
            }
            TcpExposure::Wildcard => {
                if !site.allow_lan {
                    errors.push(
                        "site.bind.lan.not_allowed",
                        "site.bind is a wildcard TCP address but site.allow_lan is false",
                        "set site.allow_lan = true",
                    );
                }
                if !site.allow_wildcard_lan {
                    errors.push(
                        "site.bind.wildcard.not_allowed",
                        "site.bind binds ALL interfaces (0.0.0.0 / ::) but site.allow_wildcard_lan is false",
                        "set site.allow_wildcard_lan = true to confirm an all-interfaces, unauthenticated bind",
                    );
                }
            }
            TcpExposure::Public => {
                errors.push(
                    "site.bind.public",
                    "site.bind is a public/global TCP address; the unauthenticated site protocol must \
                     not be exposed publicly",
                    "bind a loopback or trusted-LAN address (RFC1918 / 100.64.0.0/10 / fc00::/7)",
                );
            }
        }
    }

    fn validate_database(&self, errors: &mut ValidationErrors) {
        let database = &self.database;
        for (label, value) in [
            ("database.name", &database.name),
            ("database.admin_user", &database.admin_user),
            ("database.admin_database", &database.admin_database),
            ("database.writer_user", &database.writer_user),
            ("database.read_user", &database.read_user),
            ("database.owner_role", &database.owner_role),
        ] {
            if value.trim().is_empty() {
                errors.push(
                    "database.field.empty",
                    format!("{label} must not be empty"),
                    format!("set {label}"),
                );
            }
        }

        if !database.unsafe_single_role {
            let roles = [
                &database.read_user,
                &database.writer_user,
                &database.owner_role,
            ];
            let distinct = roles
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len();
            if distinct != roles.len() {
                errors.push(
                    "database.roles.not_distinct",
                    "database.read_user, writer_user, and owner_role must be distinct",
                    "give each role a unique name, or set database.unsafe_single_role = true (test only)",
                );
            }
        }

        if let Some(path) = &database.admin_password_file {
            require_absolute(
                errors,
                "database.password_file.relative",
                "database.admin_password_file",
                path,
            );
            // Permission check only when the file exists; absence is fine (peer/ident/.pgpass paths).
            if path.exists() {
                match secret::is_world_or_group_accessible(path) {
                    Ok(true) => errors.push(
                        "database.password_file.world_readable",
                        format!(
                            "database.admin_password_file `{}` is group/world-accessible",
                            path.display()
                        ),
                        format!("chmod 0600 {}", path.display()),
                    ),
                    Ok(false) => {}
                    Err(error) => errors.push(
                        "database.password_file.unreadable",
                        format!(
                            "could not stat database.admin_password_file `{}`: {error}",
                            path.display()
                        ),
                        "ensure the path exists and is readable by the validating user",
                    ),
                }
            }
        }
    }

    fn validate_sync(&self, errors: &mut ValidationErrors) {
        let sync = &self.sync;
        require_absolute(
            errors,
            "sync.source_root.relative",
            "sync.source_root",
            &sync.source_root,
        );
        if sync.corpora.is_empty() {
            errors.push(
                "sync.corpora.empty",
                "sync.corpora must list at least one corpus",
                "set sync.corpora = [\"core\"]",
            );
        }
        if sync.corpora.iter().any(|corpus| corpus.trim().is_empty()) {
            errors.push(
                "sync.corpora.blank_entry",
                "sync.corpora must not contain blank entries",
                "remove the empty corpus token",
            );
        }
    }

    fn validate_trust(&self, errors: &mut ValidationErrors) {
        let anchors = &self.trust.anchor;
        let package_count = anchors
            .iter()
            .filter(|anchor| anchor.purpose == TrustPurpose::Package)
            .count();
        let license_count = anchors
            .iter()
            .filter(|anchor| anchor.purpose == TrustPurpose::License)
            .count();

        if package_count == 0 {
            errors.push(
                "trust.package_anchor.missing",
                "at least one [[trust.anchor]] with purpose = \"package\" is required",
                "add the producer's package verifying key as a [[trust.anchor]]",
            );
        }
        if self.license.is_some() && license_count == 0 {
            errors.push(
                "trust.license_anchor.missing",
                "a [license] token is configured but no [[trust.anchor]] with purpose = \"license\" exists",
                "add the license issuer's verifying key as a license-purpose [[trust.anchor]]",
            );
        }

        for (index, anchor) in anchors.iter().enumerate() {
            if anchor.key_id.trim().is_empty() {
                errors.push(
                    "trust.anchor.key_id.empty",
                    format!("[[trust.anchor]] #{index} has an empty key_id"),
                    "set the producer-supplied key_id",
                );
            }
            if !anchor.algorithm.eq_ignore_ascii_case("ed25519") {
                errors.push(
                    "trust.anchor.algorithm.unsupported",
                    format!(
                        "[[trust.anchor]] #{index} algorithm `{}` is unsupported",
                        anchor.algorithm
                    ),
                    "use algorithm = \"ed25519\"",
                );
            }
            if !is_hex_len(&anchor.public_key_hex, 64) {
                errors.push(
                    "trust.anchor.public_key.invalid",
                    format!(
                        "[[trust.anchor]] #{index} public_key_hex must be 64 hex chars (32-byte ed25519 key)"
                    ),
                    "paste the 64-character hex public key from the producer",
                );
            }
        }
    }

    fn validate_embedder(&self, errors: &mut ValidationErrors) {
        let embedder = &self.embedder;

        if embedder.provider != EmbeddingProvider::OpenAiCompatible {
            errors.push(
                "embedder.provider.unsupported",
                "site embedder.provider must be \"openai_compatible\" (the local bge-m3 endpoint)",
                "set embedder.provider = \"openai_compatible\"",
            );
        }

        // LOOPBACK-ONLY guard (confidentiality boundary). Reuse the embed crate's classifier.
        if base_url_class(&embedder.base_url) != BaseUrlClass::LocalLoopback {
            errors.push(
                "embedder.base_url.not_loopback",
                format!(
                    "site query embedder base_url `{}` is not loopback; customer query text must NOT \
                     leave the host (no OpenRouter / external provider for site queries)",
                    embedder.base_url
                ),
                "point embedder.base_url at localhost / 127.0.0.1 / ::1 (the local bge-m3 endpoint)",
            );
        } else {
            // Parse the loopback base_url once: it must be an http(s) URL, and its EFFECTIVE port
            // (explicit, or the scheme default) must match the managed bge-m3 `embedder.port`.
            // Using the effective port closes the gap where `http://127.0.0.1` (default port 80)
            // silently diverged from `--port 8081`.
            match url::Url::parse(&embedder.base_url) {
                Ok(url) => {
                    let scheme = url.scheme();
                    if scheme != "http" && scheme != "https" {
                        errors.push(
                            "embedder.base_url.scheme",
                            format!("embedder.base_url scheme `{scheme}` must be http or https"),
                            "use http://127.0.0.1:<port> (the local bge-m3 endpoint)",
                        );
                    }
                    if let Some(effective_port) = url.port_or_known_default()
                        && effective_port != embedder.port
                    {
                        errors.push(
                            "embedder.port.mismatch",
                            format!(
                                "embedder.base_url effective port {effective_port} does not match embedder.port {}",
                                embedder.port
                            ),
                            "make embedder.port equal the port in embedder.base_url (add an explicit `:<port>` if you rely on a scheme default)",
                        );
                    }
                }
                Err(error) => errors.push(
                    "embedder.base_url.malformed",
                    format!(
                        "embedder.base_url `{}` is not a valid URL: {error}",
                        embedder.base_url
                    ),
                    "use http://127.0.0.1:<port> (the local bge-m3 endpoint)",
                ),
            }
        }

        if !embedder.pooling.eq_ignore_ascii_case(REQUIRED_POOLING) {
            errors.push(
                "embedder.pooling.unsupported",
                format!(
                    "embedder.pooling `{}` is unsupported in this phase",
                    embedder.pooling
                ),
                "set embedder.pooling = \"cls\"",
            );
        }

        if embedder.dimension == 0 {
            errors.push(
                "embedder.dimension.zero",
                "embedder.dimension must be > 0",
                "set embedder.dimension (bge-m3 = 1024)",
            );
        }
        if embedder.model_name.trim().is_empty() {
            errors.push(
                "embedder.model_name.empty",
                "embedder.model_name must not be empty",
                "set embedder.model_name (e.g. \"bge-m3\")",
            );
        }

        require_absolute(
            errors,
            "embedder.llama_server.relative",
            "embedder.llama_server",
            &embedder.llama_server,
        );
        require_absolute(
            errors,
            "embedder.model_path.relative",
            "embedder.model_path",
            &embedder.model_path,
        );
        require_absolute(
            errors,
            "embedder.tokenizer_json.relative",
            "embedder.tokenizer_json",
            &embedder.tokenizer_json,
        );
    }

    /// THE central encoding boundary for everything rendered into a generated env file or systemd
    /// unit (`render.rs`). It runs over EVERY string/path value that `render` emits verbatim, so a
    /// future rendered field is covered by adding it to one of the lists below — not by sprinkling
    /// ad-hoc checks at each call site.
    ///
    /// Guarantee established here: no accepted config value can split a rendered `KEY=value` line,
    /// inject a second env line (e.g. a hosted `JURISEARCH_EMBED_BASE_URL=` after the validated
    /// loopback one), or add/forge a flag or directive in an `ExecStart`/unit line — AND no value
    /// bound to an `ExecStart` argv token (inlined as a command word, or expanded via `${VAR}`) can
    /// split into extra argv words or trigger nested expansion. Identifiers use a conservative
    /// allowlist; free text and paths must be single ARGV-SAFE tokens (no control chars, no ASCII
    /// whitespace, no systemd expansion/quoting metacharacters). Every text/path value below either
    /// reaches an `ExecStart` argv token or a unit directive, and none legitimately contains
    /// internal whitespace for this deployment, so the argv-safe rule is applied uniformly.
    fn validate_render_safety(&self, errors: &mut ValidationErrors) {
        // Identifiers: rendered as env values AND inlined unquoted into unit `ExecStart` lines
        // (service user/group, DB names/roles, corpus tokens). Restrict to `[A-Za-z0-9._-]`.
        let identifiers: [(&str, &str); 8] = [
            ("system.service_user", self.system.service_user.as_str()),
            ("system.service_group", self.system.service_group.as_str()),
            ("database.name", self.database.name.as_str()),
            ("database.admin_user", self.database.admin_user.as_str()),
            (
                "database.admin_database",
                self.database.admin_database.as_str(),
            ),
            ("database.writer_user", self.database.writer_user.as_str()),
            ("database.read_user", self.database.read_user.as_str()),
            ("database.owner_role", self.database.owner_role.as_str()),
        ];
        for (label, value) in identifiers {
            require_identifier(errors, label, value);
        }
        for (index, corpus) in self.sync.corpora.iter().enumerate() {
            require_identifier(errors, &format!("sync.corpora[{index}]"), corpus);
        }

        // Free-text values rendered verbatim into env-file `KEY=value` lines AND, for the
        // argv-bound ones, expanded as separate `${VAR}` words inside `ExecStart`:
        //   - `database.host`  -> `${JURISEARCH_DB_HOST}` (site + syncd ExecStart)
        //   - `site.bind`      -> `${JURISEARCH_SITE_BIND}` (site ExecStart; rendered token is a
        //                          substring of this raw string, so guarding the raw value covers it)
        //   - `embedder.pooling` -> `${JURISEARCH_BGE_M3_POOLING}` (bge-m3 ExecStart)
        // `embedder.base_url` / `embedder.model_name` are env-only, but none of these legitimately
        // contains whitespace, so all get the single-token ARGV-SAFE rule (reject control chars,
        // ASCII whitespace, and systemd expansion/quoting metacharacters).
        let text_values: [(&str, &str); 5] = [
            ("database.host", self.database.host.as_str()),
            ("site.bind", self.site.bind.as_str()),
            ("embedder.base_url", self.embedder.base_url.as_str()),
            ("embedder.model_name", self.embedder.model_name.as_str()),
            ("embedder.pooling", self.embedder.pooling.as_str()),
        ];
        for (label, value) in text_values {
            require_argv_safe(errors, label, value);
        }

        // Paths rendered into env values, `EnvironmentFile=`/`ReadOnlyPaths=` literals, OR inlined as
        // an `ExecStart` binary / expanded as an `${VAR}` argv word (`system.install_dir` ->
        // `{dir}/jurisearch`, `embedder.llama_server` binary, `sync.source_root` ->
        // `${JURISEARCH_SOURCE_ROOT}`, `embedder.model_path` -> `${JURISEARCH_BGE_M3_MODEL}`).
        // Absolute-ness is checked elsewhere; here every path must be a single ARGV-SAFE token.
        let mut paths: Vec<(&str, &Path)> = vec![
            ("system.install_dir", self.system.install_dir.as_path()),
            ("system.config_dir", self.system.config_dir.as_path()),
            ("system.runtime_dir", self.system.runtime_dir.as_path()),
            ("system.state_dir", self.system.state_dir.as_path()),
            ("sync.source_root", self.sync.source_root.as_path()),
            (
                "embedder.llama_server",
                self.embedder.llama_server.as_path(),
            ),
            ("embedder.model_path", self.embedder.model_path.as_path()),
            (
                "embedder.tokenizer_json",
                self.embedder.tokenizer_json.as_path(),
            ),
        ];
        if let Some(path) = &self.database.admin_password_file {
            paths.push(("database.admin_password_file", path.as_path()));
        }
        if let Some(license) = &self.license {
            paths.push(("license.token_json", license.token_json.as_path()));
        }
        for (label, path) in paths {
            require_path_argv_safe(errors, label, path);
        }
    }
}

/// A character that, if present in a value rendered into an `ExecStart` argv token, would let the
/// value break out of its single token: ASCII/Unicode whitespace (systemd word-splits `${VAR}`
/// expansions on whitespace) or a systemd expansion/quoting metacharacter. `$` blocks nested
/// `${VAR}`/`$VAR` expansion; `"`, `'`, `\`, backtick and `;` block quote/escape break-outs.
/// (Control whitespace such as tab/newline is reported separately as a control character.)
fn is_argv_unsafe(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '$' | '"' | '\'' | '\\' | '`' | ';')
}

fn require_absolute(errors: &mut ValidationErrors, code: &'static str, label: &str, path: &Path) {
    if !path.is_absolute() {
        errors.push(
            code,
            format!("{label} `{}` must be an absolute path", path.display()),
            format!("use an absolute path for {label}"),
        );
    }
}

fn is_hex_len(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Reject a free-text value rendered into an env/unit file unless it is a single ARGV-SAFE token.
/// Control characters/newlines (which could split or inject a line) are reported first; ASCII
/// whitespace and systemd expansion/quoting metacharacters (which would forge an extra `ExecStart`
/// argv word when the value is expanded via `${VAR}`) are rejected next.
fn require_argv_safe(errors: &mut ValidationErrors, label: &str, value: &str) {
    if value.chars().any(char::is_control) {
        errors.push(
            "render.value.control_char",
            format!(
                "{label} must not contain control characters or newlines (it is rendered verbatim \
                 into a generated env/unit file)"
            ),
            format!("remove the embedded newline/control character from {label}"),
        );
    } else if value.chars().any(is_argv_unsafe) {
        errors.push(
            "render.value.argv_unsafe",
            format!(
                "{label} `{value}` must be a single token: it is used as an `ExecStart` argument \
                 and must not contain whitespace or shell/systemd metacharacters ($ \" ' \\ ` ;)"
            ),
            format!("remove the whitespace/metacharacter from {label} (it must be one argv token)"),
        );
    }
}

/// Require a conservative identifier (`[A-Za-z0-9._-]`, non-empty) for values inlined unquoted into
/// unit `ExecStart` lines or env values, so they can never split a line, inject a flag, or add a
/// directive.
fn require_identifier(errors: &mut ValidationErrors, label: &str, value: &str) {
    let valid = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid {
        errors.push(
            "render.identifier.invalid",
            format!("{label} `{value}` must be a non-empty identifier limited to [A-Za-z0-9._-]"),
            format!("use only letters, digits, `.`, `_`, or `-` for {label}"),
        );
    }
}

/// Reject a path rendered into a unit/env file unless its string form is a single ARGV-SAFE token.
/// Control characters are reported first (line injection); ASCII whitespace and systemd
/// expansion/quoting metacharacters are rejected next (extra argv word / nested expansion when the
/// path is inlined as an `ExecStart` binary or expanded via `${VAR}`).
fn require_path_argv_safe(errors: &mut ValidationErrors, label: &str, path: &Path) {
    let value = path.to_string_lossy();
    if value.chars().any(char::is_control) {
        errors.push(
            "render.path.control_char",
            format!(
                "{label} `{}` must not contain control characters or newlines",
                path.display()
            ),
            format!("remove the embedded newline/control character from {label}"),
        );
    } else if value.chars().any(is_argv_unsafe) {
        errors.push(
            "render.path.argv_unsafe",
            format!(
                "{label} `{}` must be a single token: it is used as an `ExecStart` path/argument \
                 and must not contain whitespace or shell/systemd metacharacters ($ \" ' \\ ` ;)",
                path.display()
            ),
            format!("remove the whitespace/metacharacter from {label} (it must be one argv token)"),
        );
    }
}
