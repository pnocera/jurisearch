//! Parse + classify the `site.bind` string into the runtime flag shape `serve-site` expects.
//!
//! `tcp://host:port` -> `--tcp host:port`; `unix:///absolute/path` -> `--socket /absolute/path`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A parsed `site.bind` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindAddress {
    /// A TCP bind: the literal `host:port` token and its exposure classification.
    Tcp {
        /// The exact `host:port` token passed to `serve-site --tcp` (host re-bracketed for IPv6).
        host_port: String,
        host: String,
        port: u16,
        exposure: TcpExposure,
    },
    /// A Unix-domain socket bind at an absolute path.
    Unix { path: String },
}

/// How exposed a TCP bind is, which gates the `allow_lan` / `allow_wildcard_lan` flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpExposure {
    /// `127.0.0.0/8`, `::1`, or `localhost` — bindable with no extra flag.
    Loopback,
    /// A trusted-LAN range (RFC1918 / 100.64.0.0/10 CGNAT / fc00::/7 ULA) — needs `allow_lan`.
    TrustedLan,
    /// A wildcard (`0.0.0.0` / `::`) — needs `allow_lan` AND `allow_wildcard_lan`.
    Wildcard,
    /// Anything else (a public/global address) — refused outright.
    Public,
}

/// Why a `site.bind` string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindParseError {
    UnknownScheme,
    MissingHost,
    MissingPort,
    UnixNotAbsolute,
    Malformed(String),
}

impl std::fmt::Display for BindParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindParseError::UnknownScheme => write!(
                formatter,
                "must start with `tcp://` or `unix:///` (an absolute unix path)"
            ),
            BindParseError::MissingHost => write!(formatter, "tcp bind is missing a host"),
            BindParseError::MissingPort => write!(formatter, "tcp bind is missing a port"),
            BindParseError::UnixNotAbsolute => {
                write!(
                    formatter,
                    "unix socket path must be absolute (`unix:///...`)"
                )
            }
            BindParseError::Malformed(detail) => write!(formatter, "{detail}"),
        }
    }
}

/// Parse a `site.bind` string. Pure: does no IO and no policy enforcement (that is the validator's
/// job); it only resolves the shape and classifies a TCP host's exposure.
pub fn parse_bind(bind: &str) -> Result<BindAddress, BindParseError> {
    if let Some(rest) = bind.strip_prefix("tcp://") {
        return parse_tcp(rest);
    }
    if let Some(rest) = bind.strip_prefix("unix://") {
        // `unix:///srv/x.sock` -> rest is `/srv/x.sock`.
        if !rest.starts_with('/') {
            return Err(BindParseError::UnixNotAbsolute);
        }
        return Ok(BindAddress::Unix {
            path: rest.to_owned(),
        });
    }
    Err(BindParseError::UnknownScheme)
}

fn parse_tcp(rest: &str) -> Result<BindAddress, BindParseError> {
    // IPv6 literal form: [::1]:8099
    let (host, port_str) = if let Some(after_bracket) = rest.strip_prefix('[') {
        let close = after_bracket
            .find(']')
            .ok_or_else(|| BindParseError::Malformed("unterminated IPv6 `[` in bind".to_owned()))?;
        let host = &after_bracket[..close];
        let tail = &after_bracket[close + 1..];
        let port_str = tail.strip_prefix(':').ok_or(BindParseError::MissingPort)?;
        (host.to_owned(), port_str)
    } else {
        let (host, port_str) = rest.rsplit_once(':').ok_or(BindParseError::MissingPort)?;
        (host.to_owned(), port_str)
    };

    if host.is_empty() {
        return Err(BindParseError::MissingHost);
    }
    if port_str.is_empty() {
        return Err(BindParseError::MissingPort);
    }
    let port: u16 = port_str
        .parse()
        .map_err(|_| BindParseError::Malformed(format!("invalid port `{port_str}` in bind")))?;

    let exposure = classify_host(&host);
    // Re-bracket an IPv6 host so the emitted `host:port` token is unambiguous.
    let host_port = if host.parse::<Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };

    Ok(BindAddress::Tcp {
        host_port,
        host,
        port,
        exposure,
    })
}

/// Classify a TCP host string's exposure for the allow-flag policy.
#[must_use]
pub fn classify_host(host: &str) -> TcpExposure {
    if host.eq_ignore_ascii_case("localhost") {
        return TcpExposure::Loopback;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => classify_ipv4(v4),
        Ok(IpAddr::V6(v6)) => classify_ipv6(v6),
        // A non-IP, non-localhost domain is treated as public (refused) for a no-auth service.
        Err(_) => TcpExposure::Public,
    }
}

fn classify_ipv4(address: Ipv4Addr) -> TcpExposure {
    if address.is_loopback() {
        return TcpExposure::Loopback;
    }
    if address.is_unspecified() {
        return TcpExposure::Wildcard;
    }
    let [a, b, ..] = address.octets();
    // RFC1918 private ranges + 100.64.0.0/10 CGNAT (the Tailscale range).
    let private = address.is_private();
    let cgnat = a == 100 && (64..=127).contains(&b);
    if private || cgnat {
        TcpExposure::TrustedLan
    } else {
        TcpExposure::Public
    }
}

fn classify_ipv6(address: Ipv6Addr) -> TcpExposure {
    if address.is_loopback() {
        return TcpExposure::Loopback;
    }
    if address.is_unspecified() {
        return TcpExposure::Wildcard;
    }
    // fc00::/7 Unique Local Addresses (first byte 0xfc or 0xfd).
    let first = address.octets()[0];
    if first == 0xfc || first == 0xfd {
        TcpExposure::TrustedLan
    } else {
        TcpExposure::Public
    }
}
