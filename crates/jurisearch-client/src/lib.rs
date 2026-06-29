//! work/09 P6 — the thin client library: resolve a site service endpoint from a URL, dial it
//! (TCP/UDS), and run one request through the versioned [`JsonlClient`], returning the
//! [`SessionResponse`]. Rendering + CLI live in the binary; this owns endpoint policy + dialing.
//!
//! Dependency-light by construction: `jurisearch-core` (contract/envelope) + `jurisearch-transport`
//! (codec/JsonlClient) + (in the binary) `jurisearch-render` — NEVER the storage/embed/ingest/CLI stack.
//! A `cargo tree` dependency-cone test enforces that the heavy crates stay out of this artifact.

use std::io::BufReader;
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use jurisearch_core::envelope::PROTOCOL_VERSION;
use jurisearch_core::session::{SessionRequest, SessionResponse};
use jurisearch_transport::{JsonlClient, TransportError};

pub mod config;
pub use config::{ClientConfig, load_config_at, resolve_config_path, save_config_at};

const READ_TIMEOUT: Duration = Duration::from_secs(120);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// The default local site socket name under `$XDG_RUNTIME_DIR` (the `--local` shorthand).
const LOCAL_SOCKET_NAME: &str = "jurisearch-site.sock";

/// A site service endpoint, parsed from an explicit URL — `tcp://host:port` or `unix:///absolute/path`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteEndpoint {
    Tcp(String),
    Unix(PathBuf),
}

impl SiteEndpoint {
    /// A stable URL-shaped description for diagnostics.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            SiteEndpoint::Tcp(addr) => format!("tcp://{addr}"),
            SiteEndpoint::Unix(path) => format!("unix://{}", path.display()),
        }
    }
}

/// A thin-client error, with a clear operator-facing message (unreachable, protocol skew, bad endpoint).
/// A server-side handler error is NOT a `ClientError` — it rides back as an `Err`-variant
/// [`SessionResponse`] the caller renders.
#[derive(Debug)]
pub enum ClientError {
    /// The `--server`/URL/`--local` selection was missing or malformed.
    BadEndpoint(String),
    /// The site service could not be reached (connection refused, no such socket, timeout).
    Unreachable {
        endpoint: String,
        source: std::io::Error,
    },
    /// The server speaks a different protocol version, or returned an unversioned (old-server) reply.
    ProtocolSkew(String),
    /// Any other transport/framing failure.
    Transport(TransportError),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::BadEndpoint(message) => write!(f, "{message}"),
            ClientError::Unreachable { endpoint, source } => {
                write!(f, "cannot reach the site service at {endpoint}: {source}")
            }
            ClientError::ProtocolSkew(message) => write!(f, "{message}"),
            ClientError::Transport(error) => write!(f, "site protocol error: {error}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Parse an explicit site URL into a [`SiteEndpoint`]. Only `tcp://host:port` and
/// `unix:///absolute/path` are accepted — a bare `host:port` is rejected so the transport is always
/// explicit and copyable into configs/runbooks.
pub fn parse_endpoint(url: &str) -> Result<SiteEndpoint, ClientError> {
    if let Some(rest) = url.strip_prefix("tcp://") {
        // Require the `host:port` SHAPE (a non-empty host + a numeric port). Hostnames are allowed
        // (resolved at dial time); IPv6 (`[::1]:8099`) splits on the LAST ':'. A missing/empty port or
        // host is a malformed-endpoint error, not a deferred "unreachable".
        let (host, port) = rest.rsplit_once(':').ok_or_else(|| {
            ClientError::BadEndpoint(format!(
                "tcp:// URL `{url}` is missing a `:port` (use tcp://host:port)"
            ))
        })?;
        if host.is_empty() {
            return Err(ClientError::BadEndpoint(format!(
                "tcp:// URL `{url}` is missing a host"
            )));
        }
        if port.parse::<u16>().is_err() {
            return Err(ClientError::BadEndpoint(format!(
                "tcp:// URL `{url}` has an invalid port `{port}` (expected 1–65535)"
            )));
        }
        Ok(SiteEndpoint::Tcp(rest.to_owned()))
    } else if let Some(rest) = url.strip_prefix("unix://") {
        // Require an ABSOLUTE path (`unix:///absolute/path`), so a copyable URL never resolves against a
        // process-relative cwd. An empty remainder is also non-absolute (rejected here).
        let path = PathBuf::from(rest);
        if !path.is_absolute() {
            return Err(ClientError::BadEndpoint(format!(
                "unix:// URL `{url}` must use an ABSOLUTE path (e.g. unix:///run/jurisearch/site.sock)"
            )));
        }
        Ok(SiteEndpoint::Unix(path))
    } else {
        Err(ClientError::BadEndpoint(format!(
            "unsupported site URL `{url}`: use tcp://host:port or unix:///absolute/path"
        )))
    }
}

/// Resolve the endpoint from the CLI selection: `--local` (a local `serve-site` UDS under
/// `$XDG_RUNTIME_DIR`), else an explicit `--server <url>`, else the `JURISEARCH_SITE_URL` environment
/// default. `--local` with no `$XDG_RUNTIME_DIR` is a clear error (it asks for `--server unix:///path`
/// rather than guessing a shared `/tmp` path).
///
/// This is the env-only resolver (no persistent config). The binary uses
/// [`resolve_endpoint_with_config`] to add the `configure`d `client.toml` as a fallback STRICTLY below
/// the env var.
pub fn resolve_endpoint(server: Option<&str>, local: bool) -> Result<SiteEndpoint, ClientError> {
    resolve_endpoint_with_config(server, local, None)
}

/// Like [`resolve_endpoint`], but with the persistent `configure`d URL as the LOWEST-priority fallback.
///
/// Precedence (highest first): `--local` / `--server` (explicit flags), then `$JURISEARCH_SITE_URL`,
/// then `configured` (the `client.toml` `server`). This preserves the existing flag/env behavior exactly
/// and slots the persisted config strictly below the env var, so a one-time `configure` removes the need
/// for shell-profile editing without ever overriding an explicit selection.
pub fn resolve_endpoint_with_config(
    server: Option<&str>,
    local: bool,
    configured: Option<&str>,
) -> Result<SiteEndpoint, ClientError> {
    if local {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
            ClientError::BadEndpoint(
                "--local needs $XDG_RUNTIME_DIR to locate the local site socket; pass \
                 --server unix:///path instead"
                    .to_owned(),
            )
        })?;
        return Ok(SiteEndpoint::Unix(
            PathBuf::from(runtime).join(LOCAL_SOCKET_NAME),
        ));
    }
    if let Some(url) = server {
        return parse_endpoint(url);
    }
    // Presence semantics MUST match `endpoint_selector_present` (`var_os`): a PRESENT
    // `$JURISEARCH_SITE_URL` is a selection and is consumed HERE, so it never silently falls through to
    // the config. A present-but-non-Unicode value (Unix) is a hard endpoint error — not "absent" — and an
    // unparseable value is the parse error from `parse_endpoint`. Only a truly ABSENT env var falls
    // through to the configured URL below.
    if let Some(value) = std::env::var_os("JURISEARCH_SITE_URL") {
        let url = value.to_str().ok_or_else(|| {
            ClientError::BadEndpoint(
                "$JURISEARCH_SITE_URL is set but is not valid UTF-8; set it to tcp://host:port or \
                 unix:///absolute/path"
                    .to_owned(),
            )
        })?;
        return parse_endpoint(url);
    }
    if let Some(url) = configured {
        return parse_endpoint(url);
    }
    Err(ClientError::BadEndpoint(
        "no site service selected: pass --server <url>, --local, set JURISEARCH_SITE_URL, or run \
         `jurisearch-client configure --server <url>`"
            .to_owned(),
    ))
}

/// Whether a HIGHER-priority endpoint selector outranks the persisted `client.toml`: `--local`,
/// `--server <url>`, or a PRESENT `$JURISEARCH_SITE_URL`. When this is `true` the config is neither
/// needed nor consulted, so an explicit selection is never hostage to a stale/malformed fallback file.
///
/// Note the env check is presence-only (`var_os`): a present-but-MALFORMED `$JURISEARCH_SITE_URL` still
/// counts as a selection, so it surfaces as an endpoint error in
/// [`resolve_endpoint_with_config`] rather than silently falling through to the config. This is the
/// single source of truth the forward and `doctor` paths share so they cannot disagree.
#[must_use]
pub fn endpoint_selector_present(server: Option<&str>, local: bool) -> bool {
    local || server.is_some() || std::env::var_os("JURISEARCH_SITE_URL").is_some()
}

/// Map a transport error into a client error, turning a version mismatch / unversioned reply into a
/// clear PROTOCOL SKEW message (the thin client speaks ONLY the versioned site protocol).
fn map_transport_error(error: TransportError) -> ClientError {
    match error {
        TransportError::Unversioned => ClientError::ProtocolSkew(format!(
            "the site service returned an UNVERSIONED reply (this client speaks site protocol v{}); \
             the server is too old or is not a jurisearch site service",
            PROTOCOL_VERSION.0
        )),
        TransportError::UnsupportedVersion { got, supported } => {
            ClientError::ProtocolSkew(format!(
                "protocol skew: the site service speaks v{got}, this client speaks v{supported}; upgrade \
             the older peer"
            ))
        }
        other => ClientError::Transport(other),
    }
}

/// Send one request to the site service and return its response. Connection failures map to
/// [`ClientError::Unreachable`]; a protocol-version mismatch maps to [`ClientError::ProtocolSkew`]. The
/// returned `SessionResponse` may itself be an `Err`-variant (a server-side handler outcome) — that is
/// NOT a client error and is rendered by the caller.
pub fn send_request(
    endpoint: &SiteEndpoint,
    request: &SessionRequest,
) -> Result<SessionResponse, ClientError> {
    let describe = endpoint.describe();
    match endpoint {
        SiteEndpoint::Tcp(addr) => {
            let stream = TcpStream::connect(addr).map_err(|source| ClientError::Unreachable {
                endpoint: describe.clone(),
                source,
            })?;
            let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
            let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
            let read_half = stream
                .try_clone()
                .map_err(|source| ClientError::Unreachable {
                    endpoint: describe,
                    source,
                })?;
            let mut client = JsonlClient::new(BufReader::new(read_half), stream);
            client.request(request).map_err(map_transport_error)
        }
        SiteEndpoint::Unix(path) => {
            let stream = UnixStream::connect(path).map_err(|source| ClientError::Unreachable {
                endpoint: describe.clone(),
                source,
            })?;
            let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
            let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
            let read_half = stream
                .try_clone()
                .map_err(|source| ClientError::Unreachable {
                    endpoint: describe,
                    source,
                })?;
            let mut client = JsonlClient::new(BufReader::new(read_half), stream);
            client.request(request).map_err(map_transport_error)
        }
    }
}

/// The `status` handshake request the `doctor` uses to probe a site service: the narrowest site
/// operation (no args), so a green reply proves connectivity AND a matching protocol version AND a
/// live query service — without mutating anything.
#[must_use]
pub fn status_probe_request() -> SessionRequest {
    SessionRequest {
        id: Some(serde_json::Value::String(
            "jurisearch-client-doctor".to_owned(),
        )),
        command: "status".to_owned(),
        args: serde_json::Value::Object(serde_json::Map::new()),
    }
}

/// Interpret the outcome of a [`status_probe_request`] sent via [`send_request`] into a
/// `(healthy, diagnostic line)` pair the `doctor` prints. A transport-level failure (unreachable /
/// protocol skew) is unhealthy with an actionable line; a SERVED error response is unhealthy too (the
/// service answered but the status operation failed); a served OK response is healthy.
#[must_use]
pub fn diagnose_status_probe(
    endpoint: &str,
    outcome: &Result<SessionResponse, ClientError>,
) -> (bool, String) {
    match outcome {
        Ok(response) if response.is_ok() => (
            true,
            format!(
                "site service at {endpoint} answered `status` over site protocol v{} (OK)",
                PROTOCOL_VERSION.0
            ),
        ),
        Ok(response) => {
            let detail = response.error().map_or_else(
                || "an error response".to_owned(),
                |error| error.message.clone(),
            );
            (
                false,
                format!(
                    "site service at {endpoint} reachable but `status` returned an error: {detail}"
                ),
            )
        }
        Err(error @ ClientError::Unreachable { .. }) => (
            false,
            format!("{error} (is the site service running and is the URL correct?)"),
        ),
        Err(error @ ClientError::ProtocolSkew(_)) => (false, format!("{error}")),
        Err(error) => (false, format!("{error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_the_two_explicit_url_schemes() {
        assert_eq!(
            parse_endpoint("tcp://127.0.0.1:8099").unwrap(),
            SiteEndpoint::Tcp("127.0.0.1:8099".to_owned())
        );
        assert_eq!(
            parse_endpoint("unix:///run/jurisearch/site.sock").unwrap(),
            SiteEndpoint::Unix(PathBuf::from("/run/jurisearch/site.sock"))
        );
    }

    #[test]
    fn rejects_a_bare_or_unknown_url() {
        assert!(parse_endpoint("127.0.0.1:8099").is_err());
        assert!(parse_endpoint("http://host:8099").is_err());
        assert!(parse_endpoint("tcp://").is_err());
        assert!(parse_endpoint("unix://").is_err());
    }

    #[test]
    fn enforces_the_url_grammar() {
        // tcp:// requires host:port (a bare host, an empty host, or a non-numeric port is rejected).
        assert!(parse_endpoint("tcp://localhost").is_err(), "missing :port");
        assert!(parse_endpoint("tcp://:8099").is_err(), "missing host");
        assert!(
            parse_endpoint("tcp://host:nope").is_err(),
            "non-numeric port"
        );
        assert!(parse_endpoint("tcp://host:").is_err(), "empty port");
        // unix:// requires an ABSOLUTE path.
        assert!(
            parse_endpoint("unix://relative.sock").is_err(),
            "relative path"
        );
        assert!(
            parse_endpoint("unix://./sock").is_err(),
            "cwd-relative path"
        );
        // Well-formed forms still parse (incl. IPv6 host:port and an absolute socket).
        assert!(parse_endpoint("tcp://[::1]:8099").is_ok());
        assert!(parse_endpoint("tcp://site.local:8099").is_ok());
        assert!(parse_endpoint("unix:///run/jurisearch/site.sock").is_ok());
    }

    #[test]
    fn resolve_requires_a_selection() {
        // No --server, no --local, and (assuming) no env → a clear error.
        if std::env::var_os("JURISEARCH_SITE_URL").is_none() {
            assert!(resolve_endpoint(None, false).is_err());
        }
        assert_eq!(
            resolve_endpoint(Some("tcp://10.0.0.1:9"), false).unwrap(),
            SiteEndpoint::Tcp("10.0.0.1:9".to_owned())
        );
    }

    #[test]
    fn the_configured_url_is_used_only_below_flags_and_env() {
        // An explicit --server beats a (different) configured URL.
        assert_eq!(
            resolve_endpoint_with_config(
                Some("tcp://explicit:1"),
                false,
                Some("tcp://configured:2")
            )
            .unwrap(),
            SiteEndpoint::Tcp("explicit:1".to_owned())
        );
        // With no flag and (assuming) no env, the configured URL is the fallback.
        if std::env::var_os("JURISEARCH_SITE_URL").is_none() {
            assert_eq!(
                resolve_endpoint_with_config(None, false, Some("tcp://configured:2")).unwrap(),
                SiteEndpoint::Tcp("configured:2".to_owned())
            );
            // No flag, no env, no config → an actionable error that names `configure`.
            let error = resolve_endpoint_with_config(None, false, None).unwrap_err();
            assert!(error.to_string().contains("configure"));
        }
    }

    #[test]
    fn diagnose_reports_a_served_ok_status_as_healthy() {
        let outcome = Ok(SessionResponse::ok(
            None,
            json!({ "service": "jurisearch-site" }),
        ));
        let (healthy, line) = diagnose_status_probe("tcp://h:1", &outcome);
        assert!(healthy);
        assert!(line.contains("OK"), "line: {line}");
    }

    #[test]
    fn diagnose_reports_a_served_error_status_as_unhealthy() {
        let outcome = Ok(SessionResponse::err(
            None,
            jurisearch_core::error::ErrorObject::internal("snapshot unavailable"),
        ));
        let (healthy, line) = diagnose_status_probe("tcp://h:1", &outcome);
        assert!(!healthy);
        assert!(line.contains("snapshot unavailable"), "line: {line}");
    }

    #[test]
    fn diagnose_reports_unreachable_and_skew_as_unhealthy_with_actionable_text() {
        let unreachable = Err(ClientError::Unreachable {
            endpoint: "tcp://h:1".to_owned(),
            source: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"),
        });
        let (healthy, line) = diagnose_status_probe("tcp://h:1", &unreachable);
        assert!(!healthy);
        assert!(line.contains("cannot reach"), "line: {line}");

        let skew = Err(ClientError::ProtocolSkew(
            "protocol skew: v9 vs v1".to_owned(),
        ));
        let (healthy, line) = diagnose_status_probe("tcp://h:1", &skew);
        assert!(!healthy);
        assert!(line.contains("skew"), "line: {line}");
    }
}
