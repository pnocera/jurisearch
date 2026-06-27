//! work/09 P4 (4A) — the `serve-site` runner: bind a UDS or loopback TCP listener and serve site requests
//! sequentially through the read-role store. 4A is loopback/UDS-only (no off-host bind — that, with the
//! bounded worker/read pools, is 4B/P6); the read identity is least-privilege by construction.

use std::fs;
use std::io;
use std::net::ToSocketAddrs;

use jurisearch_core::error::ErrorObject;
use jurisearch_storage::backend::{ConnectionConfig, ReadHandle};

use crate::args::ServeSiteArgs;
use crate::output::emit_error;

use super::dispatcher::ServerContext;
use super::{build_skeleton_dispatcher, serve_site_connection};

/// Run the site query service (4A walking skeleton): construct the read-role store, build the skeleton
/// dispatcher, and serve connections sequentially over UDS or a loopback TCP socket.
pub(crate) fn run_serve_site(args: ServeSiteArgs) -> anyhow::Result<()> {
    let store = ReadHandle::new(ConnectionConfig {
        host: args.db_host,
        port: args.db_port,
        dbname: args.db_name,
        user: args.db_user,
        password: args.db_password,
        application_name: "jurisearch-site".to_owned(),
    });
    let dispatcher = build_skeleton_dispatcher();
    let ctx = ServerContext { store: &store };

    match (args.tcp.as_deref(), args.socket.as_deref()) {
        (Some(_), Some(_)) | (None, None) => emit_error(ErrorObject::bad_input(
            "serve-site requires exactly one of --tcp or --socket",
        )),
        (Some(addr), None) => {
            let resolved = addr
                .to_socket_addrs()
                .map_err(|error| anyhow::anyhow!("invalid --tcp address {addr}: {error}"))?
                .next()
                .ok_or_else(|| anyhow::anyhow!("--tcp address {addr} did not resolve"))?;
            // 4A is loopback-only: off-host exposure (the unauthenticated LAN bind) is P6.
            if !resolved.ip().is_loopback() {
                return emit_error(ErrorObject::bad_input(format!(
                    "refusing to bind non-loopback address {resolved}: the site service is \
                     loopback/UDS-only until work/09 P6 (LAN exposure)"
                )));
            }
            let listener = std::net::TcpListener::bind(resolved)
                .map_err(|error| anyhow::anyhow!("failed to bind TCP {resolved}: {error}"))?;
            eprintln!(
                "jurisearch serve-site: listening on tcp://{resolved} (versioned site protocol; read-only; sequential)"
            );
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(120)));
                let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(30)));
                let reader = io::BufReader::new(stream.try_clone()?);
                let _ = serve_site_connection(reader, stream, &dispatcher, &ctx);
            }
            Ok(())
        }
        (None, Some(path)) => {
            use std::os::unix::fs::FileTypeExt;
            use std::os::unix::net::{UnixListener, UnixStream};
            if let Ok(meta) = fs::symlink_metadata(path) {
                if !meta.file_type().is_socket() {
                    return emit_error(ErrorObject::bad_input(format!(
                        "refusing to bind: {} exists and is not a socket",
                        path.display()
                    )));
                }
                if UnixStream::connect(path).is_ok() {
                    return emit_error(ErrorObject::bad_input(format!(
                        "a server is already listening on {}",
                        path.display()
                    )));
                }
                fs::remove_file(path).map_err(|error| {
                    anyhow::anyhow!("failed to remove stale socket {}: {error}", path.display())
                })?;
            }
            let listener = UnixListener::bind(path).map_err(|error| {
                anyhow::anyhow!("failed to bind socket {}: {error}", path.display())
            })?;
            eprintln!(
                "jurisearch serve-site: listening on unix://{} (versioned site protocol; read-only; sequential)",
                path.display()
            );
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(120)));
                let _ = stream.set_write_timeout(Some(std::time::Duration::from_secs(30)));
                let reader = io::BufReader::new(stream.try_clone()?);
                let _ = serve_site_connection(reader, stream, &dispatcher, &ctx);
            }
            Ok(())
        }
    }
}
