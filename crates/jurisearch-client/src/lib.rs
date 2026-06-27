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
pub fn resolve_endpoint(server: Option<&str>, local: bool) -> Result<SiteEndpoint, ClientError> {
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
    if let Ok(url) = std::env::var("JURISEARCH_SITE_URL") {
        return parse_endpoint(&url);
    }
    Err(ClientError::BadEndpoint(
        "no site service selected: pass --server <url>, --local, or set JURISEARCH_SITE_URL"
            .to_owned(),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
