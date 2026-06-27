//! work/09 P4 (4B) — the `serve-site` runner: bind a UDS or loopback TCP listener and serve site
//! requests from a BOUNDED WORKER pool. Each accepted connection is served on its own worker thread,
//! capped by a counting semaphore so the worker count is the hard upper bound on simultaneous
//! connections AND simultaneous read-role connections (each request opens + drops ONE read snapshot —
//! this is a bounded fresh-connection model, NOT a reuse pool). A single `Send + Sync` query embedder is
//! shared across workers, with a separate semaphore bounding in-flight embeds. 4B stays loopback/UDS-only
//! (off-host LAN exposure is P6); the read identity is least-privilege by construction.

use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use jurisearch_core::error::ErrorObject;
use jurisearch_query::{QueryEmbedder, QueryEmbedding};
use jurisearch_storage::backend::{ConnectionConfig, ReadHandle};

use crate::args::ServeSiteArgs;
use crate::embedding_runtime::PreparedQueryEmbedder;
use crate::output::emit_error;

use super::dispatcher::{ServerContext, SiteDispatcher};
use super::{build_dispatcher, serve_site_connection};

const READ_TIMEOUT: Duration = Duration::from_secs(120);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

// The shared service embedder MUST be `Send + Sync` to live behind one `Arc` across worker threads
// (codex P4-4B Q3). `OpenAiCompatibleClient` (`ureq::Agent` + tokenizer) and the fingerprint strings are
// all `Send + Sync`; this assertion fails to compile if that ever regresses (then: per-worker embedders).
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<PreparedQueryEmbedder>();
    assert_send_sync::<ThrottledEmbedder>();
    assert_send_sync::<ReadHandle>();
    assert_send_sync::<SiteDispatcher>();
};

/// A blocking counting semaphore (no async runtime): `acquire` blocks until a permit is free and returns
/// an owned guard that releases on drop. Used both to bound the worker count and the in-flight embeds.
struct Semaphore {
    available: Mutex<usize>,
    released: Condvar,
}

impl Semaphore {
    fn new(permits: usize) -> Arc<Self> {
        Arc::new(Self {
            available: Mutex::new(permits.max(1)),
            released: Condvar::new(),
        })
    }

    fn acquire(self: &Arc<Self>) -> Permit {
        let mut available = self.available.lock().expect("semaphore mutex poisoned");
        while *available == 0 {
            available = self
                .released
                .wait(available)
                .expect("semaphore condvar poisoned");
        }
        *available -= 1;
        Permit {
            semaphore: Arc::clone(self),
        }
    }
}

/// An owned semaphore permit; returns the permit to the semaphore on drop.
struct Permit {
    semaphore: Arc<Semaphore>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut available = self
            .semaphore
            .available
            .lock()
            .expect("semaphore mutex poisoned");
        *available += 1;
        self.semaphore.released.notify_one();
    }
}

/// The shared service embedder: the heavy `PreparedQueryEmbedder` plus a semaphore bounding concurrent
/// embeds against the local endpoint. `embed` is only invoked on the dense/hybrid path, so a lexical-only
/// request never acquires a permit.
struct ThrottledEmbedder {
    inner: PreparedQueryEmbedder,
    embed_permits: Arc<Semaphore>,
}

impl QueryEmbedder for ThrottledEmbedder {
    fn embed(&self, text: &str) -> Result<QueryEmbedding, ErrorObject> {
        let _permit = self.embed_permits.acquire();
        QueryEmbedder::embed(&self.inner, text)
    }
}

/// An accepted site connection (UDS or loopback TCP), served identically.
enum SiteConnection {
    Tcp(TcpStream),
    Unix(UnixStream),
}

impl SiteConnection {
    fn serve(self, dispatcher: &SiteDispatcher, ctx: &ServerContext) {
        match self {
            SiteConnection::Tcp(stream) => serve_stream(stream, dispatcher, ctx),
            SiteConnection::Unix(stream) => serve_stream(stream, dispatcher, ctx),
        }
    }
}

/// Serve one accepted stream to completion: apply read/write timeouts (so a slow/idle client cannot hold
/// a worker forever), then frame → dispatch → respond until the peer hangs up.
fn serve_stream<S>(stream: S, dispatcher: &SiteDispatcher, ctx: &ServerContext)
where
    S: Read + Write + StreamControl,
{
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
    let Ok(reader_stream) = stream.try_clone_stream() else {
        return;
    };
    let reader = BufReader::new(reader_stream);
    let _ = serve_site_connection(reader, stream, dispatcher, ctx);
}

/// The small surface `serve_stream` needs beyond `Read + Write`: timeouts and a read/write split via
/// `try_clone`. Implemented for both `TcpStream` and `UnixStream` (their methods are inherent, not a
/// shared trait), so the worker code stays generic over the two transports.
trait StreamControl: Sized {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    fn try_clone_stream(&self) -> io::Result<Self>;
}

impl StreamControl for TcpStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, timeout)
    }
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        TcpStream::set_write_timeout(self, timeout)
    }
    fn try_clone_stream(&self) -> io::Result<Self> {
        TcpStream::try_clone(self)
    }
}

impl StreamControl for UnixStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        UnixStream::set_read_timeout(self, timeout)
    }
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        UnixStream::set_write_timeout(self, timeout)
    }
    fn try_clone_stream(&self) -> io::Result<Self> {
        UnixStream::try_clone(self)
    }
}

/// The shared, immutable service state every worker borrows to build a per-connection [`ServerContext`].
struct Service {
    store: Arc<ReadHandle>,
    embedder: Arc<ThrottledEmbedder>,
    dispatcher: Arc<SiteDispatcher>,
    workers: Arc<Semaphore>,
}

impl Service {
    /// Accept connections from `accept` forever, dispatching each onto a bounded worker thread. Acquiring
    /// a worker permit BEFORE accepting the next connection is the backpressure: when all workers are
    /// busy, the acceptor blocks (and the OS backlog queues pending connections).
    fn run<F>(&self, mut accept: F)
    where
        F: FnMut() -> Option<SiteConnection>,
    {
        loop {
            let permit = self.workers.acquire();
            let Some(connection) = accept() else {
                continue;
            };
            let store = Arc::clone(&self.store);
            let embedder = Arc::clone(&self.embedder);
            let dispatcher = Arc::clone(&self.dispatcher);
            thread::spawn(move || {
                // Hold the worker permit for the connection's lifetime; dropping it frees a worker slot.
                let _permit = permit;
                let ctx = ServerContext {
                    store: &*store,
                    embedder: &*embedder,
                };
                connection.serve(&dispatcher, &ctx);
            });
        }
    }
}

/// A bound site listener (UDS or loopback TCP), produced by transport-argument validation BEFORE the
/// embedding stack is touched.
enum BoundListener {
    Tcp(std::net::TcpListener, std::net::SocketAddr),
    Unix(std::os::unix::net::UnixListener, PathBuf),
}

/// Validate the mutually-exclusive transport arguments and BIND the listener. This runs before the
/// embedding stack is probed, so a malformed invocation (neither/both of `--tcp`/`--socket`), a
/// non-loopback bind, or a stale/occupied socket fails with the expected `bad_input` rather than a
/// tokenizer/endpoint error. Returns `Ok(None)` when a `bad_input` was already emitted (the caller
/// returns it), `Ok(Some(listener))` on success, `Err` for a hard bind failure.
fn bind_site_listener(
    tcp: Option<&str>,
    socket: Option<&std::path::Path>,
) -> anyhow::Result<Option<BoundListener>> {
    match (tcp, socket) {
        (Some(_), Some(_)) | (None, None) => {
            emit_error(ErrorObject::bad_input(
                "serve-site requires exactly one of --tcp or --socket",
            ))?;
            Ok(None)
        }
        (Some(addr), None) => {
            let resolved = addr
                .to_socket_addrs()
                .map_err(|error| anyhow::anyhow!("invalid --tcp address {addr}: {error}"))?
                .next()
                .ok_or_else(|| anyhow::anyhow!("--tcp address {addr} did not resolve"))?;
            // 4B is loopback-only: off-host exposure (the unauthenticated LAN bind) is P6.
            if !resolved.ip().is_loopback() {
                emit_error(ErrorObject::bad_input(format!(
                    "refusing to bind non-loopback address {resolved}: the site service is \
                     loopback/UDS-only until work/09 P6 (LAN exposure)"
                )))?;
                return Ok(None);
            }
            let listener = std::net::TcpListener::bind(resolved)
                .map_err(|error| anyhow::anyhow!("failed to bind TCP {resolved}: {error}"))?;
            Ok(Some(BoundListener::Tcp(listener, resolved)))
        }
        (None, Some(path)) => {
            use std::os::unix::fs::FileTypeExt;
            if let Ok(meta) = fs::symlink_metadata(path) {
                if !meta.file_type().is_socket() {
                    emit_error(ErrorObject::bad_input(format!(
                        "refusing to bind: {} exists and is not a socket",
                        path.display()
                    )))?;
                    return Ok(None);
                }
                if UnixStream::connect(path).is_ok() {
                    emit_error(ErrorObject::bad_input(format!(
                        "a server is already listening on {}",
                        path.display()
                    )))?;
                    return Ok(None);
                }
                fs::remove_file(path).map_err(|error| {
                    anyhow::anyhow!("failed to remove stale socket {}: {error}", path.display())
                })?;
            }
            let listener = std::os::unix::net::UnixListener::bind(path).map_err(|error| {
                anyhow::anyhow!("failed to bind socket {}: {error}", path.display())
            })?;
            Ok(Some(BoundListener::Unix(listener, path.to_path_buf())))
        }
    }
}

/// Run the site query service (4B): validate + BIND the listener FIRST, then construct the read-role
/// store + shared throttled embedder + full dispatcher, then serve connections from a bounded worker
/// pool. Binding before probing the embedding stack means a malformed invocation or a bind conflict
/// fails with the expected `bad_input`, never a tokenizer/endpoint error.
pub(crate) fn run_serve_site(args: ServeSiteArgs) -> anyhow::Result<()> {
    let workers = args.workers.max(1);
    let max_embeds = args.max_concurrent_embeds.unwrap_or(workers).max(1);

    // 1. Transport validation + bind (BEFORE the embedding stack is touched).
    let Some(bound) = bind_site_listener(args.tcp.as_deref(), args.socket.as_deref())? else {
        return Ok(());
    };

    // 2. Now construct the service. The embedder probe (model/tokenizer/endpoint) happens only after a
    //    successful bind, so it never masks an argument or bind error.
    let store = Arc::new(ReadHandle::new(ConnectionConfig {
        host: args.db_host,
        port: args.db_port,
        dbname: args.db_name,
        user: args.db_user,
        password: args.db_password,
        application_name: "jurisearch-site".to_owned(),
    }));
    let inner = match PreparedQueryEmbedder::from_env() {
        Ok(inner) => inner,
        Err(error) => return emit_error(error),
    };
    let embedder = Arc::new(ThrottledEmbedder {
        inner,
        embed_permits: Semaphore::new(max_embeds),
    });
    let dispatcher = Arc::new(build_dispatcher(workers));
    let service = Service {
        store,
        embedder,
        dispatcher,
        workers: Semaphore::new(workers),
    };

    // 3. Accept loop on the already-bound listener.
    match bound {
        BoundListener::Tcp(listener, resolved) => {
            eprintln!(
                "jurisearch serve-site: listening on tcp://{resolved} (versioned site protocol; read-only; {workers} workers)"
            );
            let mut incoming = listener.incoming();
            service.run(|| {
                loop {
                    match incoming.next() {
                        Some(Ok(stream)) => return Some(SiteConnection::Tcp(stream)),
                        Some(Err(_)) => continue,
                        None => return None,
                    }
                }
            });
        }
        BoundListener::Unix(listener, path) => {
            eprintln!(
                "jurisearch serve-site: listening on unix://{} (versioned site protocol; read-only; {workers} workers)",
                path.display()
            );
            let mut incoming = listener.incoming();
            service.run(|| {
                loop {
                    match incoming.next() {
                        Some(Ok(stream)) => return Some(SiteConnection::Unix(stream)),
                        Some(Err(_)) => continue,
                        None => return None,
                    }
                }
            });
        }
    }
    Ok(())
}
