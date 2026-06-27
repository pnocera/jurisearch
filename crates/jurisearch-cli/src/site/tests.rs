//! work/09 P4 (4B) e2e: the site service end to end over an in-memory connection, through the READ-ROLE
//! store and the FULL operation set. Proves: a versioned `fetch`/`search`/`cite` request frames →
//! dispatches → validates the writer-owned readiness stamp on its snapshot → reads under the
//! least-privilege read identity → returns the document (with render parity); a fingerprint mismatch
//! fails closed THROUGH the dispatcher; a missing readiness stamp is refused; `status` returns health;
//! `online`/`zone`/unversioned/`index_dir` are rejected. A second (no-PG) test proves concurrent requests
//! each open their OWN snapshot. Skips cleanly when the managed-PG harness is absent.

use std::io::Cursor;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use jurisearch_core::envelope::ProtocolEnvelope;
use jurisearch_core::error::ErrorObject;
use jurisearch_core::operation::Operation;
use jurisearch_core::session::{SessionRequest, SessionResponse};
use jurisearch_query::QueryEmbedder;
use jurisearch_storage::backend::{
    DEFAULT_OWNER_ROLE, DEFAULT_READ_ROLE, DEFAULT_WRITER_ROLE, ManagedPostgresBackend, RoleSpec,
    StorageBackend, provision_roles,
};
use jurisearch_storage::generations::{
    ActivationReadVisibility, ActivationStamps, CursorGuard,
    activate_generation_with_guard_and_visibility, create_generation_from_public,
    create_generation_schema, generation_schema,
};
use jurisearch_storage::query::{ActiveCorpus, QueryStore, ReadSnapshot};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_transport::{encode_bare_request_line, encode_site_envelope_line};
use serde_json::{Value, json};

use super::dispatcher::{OperationHandler, ServerContext, SiteDispatcher};
use super::testkit::{PanicEmbedder, StubEmbedder};
use super::{build_dispatcher, serve_site_connection};

fn stamps() -> ActivationStamps<'static> {
    ActivationStamps {
        sequence: 1,
        baseline_id: "core-site-g0001",
        schema_version: 24,
        embedding_fingerprint: "fp",
        builder_versions: &serde_json::Value::Null,
        last_package_id: None,
        last_package_digest: None,
    }
}

/// Seed one query-ready `core` decision, provision the least-privilege roles, and activate a generation
/// with the read role's visibility — the supported shared-server read path. Activation stamps query
/// readiness (P3A), so the site read gate passes.
fn ready_site(postgres: &ManagedPostgres) -> Result<(), StorageError> {
    {
        let mut superuser = postgres.client()?;
        provision_roles(&mut superuser, &RoleSpec::default(), &postgres.database)?;
    }
    postgres.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:SITE','cass','decision','cass:SITE','Cass','Arret du site','le corps', \
           '2024-01-01','sha256:site','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:SITE#0','cass:SITE',0,'le corps','ctx le corps','sha256:cs','c1','fp');",
    )?;
    let vector = (0..1024).map(|_| "0.01").collect::<Vec<_>>().join(",");
    postgres.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:SITE#0','fp','[{vector}]'::vector,'m',1024);"
    ))?;
    let generation = create_generation_from_public(postgres, "core", 1, Some("core-site-g0001"))?;
    activate_generation_with_guard_and_visibility(
        postgres,
        "core",
        &generation,
        &stamps(),
        CursorGuard::FirstBaseline,
        &[],
        &ActivationReadVisibility {
            read_role: DEFAULT_READ_ROLE,
            view_owner_role: DEFAULT_OWNER_ROLE,
        },
    )?;
    Ok(())
}

/// Frame one request line, run it through the site listener over the read-role store with the given
/// server embedder, and decode the single response line.
fn round_trip(
    backend: &ManagedPostgresBackend,
    embedder: &dyn QueryEmbedder,
    request_line: &str,
) -> Result<SessionResponse, StorageError> {
    let store = backend.read_handle()?;
    let ctx = ServerContext {
        store: &store,
        embedder,
    };
    let dispatcher = build_dispatcher(4);
    let mut output: Vec<u8> = Vec::new();
    serve_site_connection(
        Cursor::new(format!("{request_line}\n")),
        &mut output,
        &dispatcher,
        &ctx,
    )
    .map_err(StorageError::Io)?;
    let line = String::from_utf8(output).expect("utf8 response");
    // The site service replies with a VERSIONED response envelope (work/09 P6); unwrap it to the
    // SessionResponse the assertions check.
    let envelope = jurisearch_transport::decode_site_response_envelope_line(line.trim())
        .map_err(|error| StorageError::Io(std::io::Error::other(error.to_string())))?;
    Ok(envelope.response)
}

fn site_line(command: &str, args: serde_json::Value) -> String {
    let request = SessionRequest {
        id: Some(json!("r1")),
        command: command.to_owned(),
        args,
    };
    encode_site_envelope_line(&ProtocolEnvelope::new(request))
        .trim()
        .to_owned()
}

#[test]
fn the_site_service_serves_the_full_operation_set_through_the_read_role() -> Result<(), StorageError>
{
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-p4-site.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    ready_site(&postgres)?;
    let backend = ManagedPostgresBackend::new(
        &postgres,
        DEFAULT_READ_ROLE,
        DEFAULT_WRITER_ROLE,
        DEFAULT_OWNER_ROLE,
    );
    // The server embedder: a fixed 1024-d vector under the ACTIVE fingerprint (`fp`), so the dense path
    // is fingerprint-compatible with the seeded generation.
    let embedder = StubEmbedder {
        fingerprint: "fp".to_owned(),
        dimension: 1024,
    };

    // ---- fetch: full FetchResponse shape + render parity through the read role -----------------------
    let response = round_trip(
        &backend,
        &embedder,
        &site_line("fetch", json!({"ids": ["cass:SITE"]})),
    )?;
    assert!(response.is_ok(), "fetch ok: {response:?}");
    let documents = response.result().expect("a result")["documents"]
        .as_array()
        .expect("documents array");
    assert_eq!(documents.len(), 1);
    let document = &documents[0];
    assert_eq!(document["document_id"].as_str(), Some("cass:SITE"));
    assert_eq!(document["citation"].as_str(), Some("Cass"));
    assert_eq!(document["title"].as_str(), Some("Arret du site"));
    assert_eq!(document["body"].as_str(), Some("le corps"));
    let chunks = document["chunks"].as_array().expect("chunks array");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0]["chunk_id"].as_str(), Some("cass:SITE#0"));
    let rendered = jurisearch_render::render_session_response(&response).expect("render");
    let expected = jurisearch_render::render_value_pretty(response.result().expect("result"))
        .expect("render expected");
    assert_eq!(
        rendered, expected,
        "the site response renders with one-shot parity"
    );
    assert!(rendered.contains("cass:SITE"));

    // ---- search: hybrid retrieval through the dispatcher returns the seeded decision ----------------
    let search = round_trip(
        &backend,
        &embedder,
        &site_line(
            "search",
            json!({"query": "corps", "mode": "hybrid", "kind": "decision"}),
        ),
    )?;
    assert!(search.is_ok(), "site search ok: {search:?}");
    let candidates = search.result().expect("search result")["candidates"]
        .as_array()
        .expect("candidates array");
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate["document_id"].as_str() == Some("cass:SITE")),
        "the seeded decision is retrieved: {candidates:?}"
    );

    // ---- cite: a decision identifier classifies through the read snapshot ---------------------------
    let cite = round_trip(
        &backend,
        &embedder,
        &site_line("cite", json!({"cite": "cass:SITE"})),
    )?;
    assert!(cite.is_ok(), "site cite ok: {cite:?}");
    let cite_result = cite.result().expect("cite result");
    assert_eq!(cite_result["online"]["requested"].as_bool(), Some(false));
    assert!(
        cite_result["state"].is_string(),
        "cite reports a state: {cite_result:?}"
    );

    // ---- status: health reports topology + the readiness stamp + the bounded read model -------------
    let health = round_trip(&backend, &embedder, &site_line("status", json!({})))?;
    assert!(health.is_ok(), "status/health ok: {health:?}");
    let result = health.result().expect("health result");
    assert_eq!(result["service"].as_str(), Some("jurisearch-site"));
    assert_eq!(result["active_corpora"][0]["corpus"].as_str(), Some("core"));
    assert_eq!(
        result["multi_corpus_readiness"].as_str(),
        Some("single_corpus")
    );
    assert_eq!(
        result["readiness"]["ready"].as_bool(),
        Some(true),
        "the activated, stamped topology is ready: {result}"
    );
    assert_eq!(
        result["read_pool"]["max_read_connections"].as_u64(),
        Some(4)
    );

    // ---- boundary rejections ------------------------------------------------------------------------
    // A `zone` search is rejected (Cassation-only client concern).
    let zoned = round_trip(
        &backend,
        &embedder,
        &site_line("search", json!({"query": "corps", "zone": "motivations"})),
    )?;
    assert!(!zoned.is_ok(), "a zone search is rejected");
    assert!(
        zoned
            .error()
            .is_some_and(|error| error.message.contains("zone")),
        "the rejection names zone: {zoned:?}"
    );
    // An `online` cite is rejected (the Légifrance probe is a client/online concern).
    let online = round_trip(
        &backend,
        &embedder,
        &site_line("cite", json!({"cite": "cass:SITE", "online": true})),
    )?;
    assert!(!online.is_ok(), "an online cite is rejected");
    assert!(
        online
            .error()
            .is_some_and(|error| error.message.contains("online")),
        "the rejection names online: {online:?}"
    );
    // An UNVERSIONED frame (the local bare request shape) is rejected before dispatch.
    let bare = encode_bare_request_line(&SessionRequest {
        id: Some(json!("r2")),
        command: "fetch".to_owned(),
        args: json!({"ids": ["cass:SITE"]}),
    });
    let rejected = round_trip(&backend, &embedder, bare.trim())?;
    assert!(!rejected.is_ok(), "an unversioned site frame is rejected");
    // A client-supplied `index_dir` is rejected at the boundary (never influences the server).
    let index_dir = round_trip(
        &backend,
        &embedder,
        &site_line(
            "fetch",
            json!({"ids": ["cass:SITE"], "index_dir": "/tmp/x"}),
        ),
    )?;
    assert!(!index_dir.is_ok(), "a client index_dir is rejected");
    assert!(
        index_dir
            .error()
            .is_some_and(|error| error.message.contains("index_dir")),
        "the rejection names index_dir: {index_dir:?}"
    );

    // ---- fingerprint mismatch fails closed THROUGH the dispatcher -----------------------------------
    // A server embedder whose fingerprint disagrees with the active generation must fail the dense
    // preflight (P3A/P3C), not silently fall back to a partial result.
    let wrong = StubEmbedder {
        fingerprint: "wrong-fp".to_owned(),
        dimension: 1024,
    };
    let mismatch = round_trip(
        &backend,
        &wrong,
        &site_line(
            "search",
            json!({"query": "corps", "mode": "dense", "kind": "decision"}),
        ),
    )?;
    assert!(
        !mismatch.is_ok(),
        "a fingerprint-mismatched dense search must fail closed: {mismatch:?}"
    );

    // ---- a missing readiness stamp is refused by the read gate --------------------------------------
    // Drop the writer-owned stamp; every query handler must now fail closed (the P3A contract), even
    // though the tables are still visible.
    postgres.execute_sql("DELETE FROM public.index_manifest WHERE key = 'query_readiness';")?;
    let unready = round_trip(
        &backend,
        &embedder,
        &site_line("fetch", json!({"ids": ["cass:SITE"]})),
    )?;
    assert!(
        !unready.is_ok(),
        "a fetch on an unstamped topology is refused: {unready:?}"
    );
    // Health still answers (it never gates) and now reports not-ready.
    let health_unready = round_trip(&backend, &embedder, &site_line("status", json!({})))?;
    assert!(health_unready.is_ok(), "health answers even when not ready");
    assert_eq!(
        health_unready.result().expect("health")["readiness"]["ready"].as_bool(),
        Some(false),
        "health reports the dropped stamp as not-ready"
    );
    Ok(())
}

// ---- concurrency: each request opens its OWN snapshot (no PG) ---------------------------------------

/// A fake store that COUNTS `begin_snapshot` calls and tags each snapshot with a unique id, so a
/// concurrent dispatch can prove every request gets its own snapshot (no shared `postgres::Client`).
struct CountingStore {
    opened: Arc<AtomicUsize>,
}
impl QueryStore for CountingStore {
    fn begin_snapshot(&self) -> Result<Box<dyn ReadSnapshot + '_>, StorageError> {
        let id = self.opened.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(CountingSnapshot { id }))
    }
}
struct CountingSnapshot {
    id: usize,
}
impl ReadSnapshot for CountingSnapshot {
    fn read_text(&mut self, _sql: &str) -> Result<String, StorageError> {
        Ok(self.id.to_string())
    }
    fn read_text_for_corpus(
        &mut self,
        _corpus: &ActiveCorpus,
        _sql: &str,
    ) -> Result<String, StorageError> {
        Ok(self.id.to_string())
    }
    fn active_corpora(&self) -> &[ActiveCorpus] {
        &[]
    }
}

/// A handler that opens a snapshot and returns its unique id — bypasses the readiness gate/builders so
/// the test isolates the snapshot-per-request concurrency contract.
struct SnapshotIdHandler;
impl OperationHandler for SnapshotIdHandler {
    fn handle(&self, ctx: &ServerContext, _args: &Value) -> Result<Value, ErrorObject> {
        let mut snapshot = ctx
            .store
            .begin_snapshot()
            .map_err(|error| ErrorObject::bad_input(error.to_string()))?;
        let id = snapshot
            .read_text("")
            .map_err(|error| ErrorObject::bad_input(error.to_string()))?;
        Ok(json!({ "snapshot_id": id }))
    }
}

#[test]
fn concurrent_dispatch_opens_one_independent_snapshot_per_request() {
    const REQUESTS: usize = 16;
    let opened = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(CountingStore {
        opened: Arc::clone(&opened),
    });
    let embedder = Arc::new(PanicEmbedder);
    let mut dispatcher = SiteDispatcher::new();
    dispatcher.register(Operation::Fetch, Box::new(SnapshotIdHandler));
    let dispatcher = Arc::new(dispatcher);

    let handles: Vec<_> = (0..REQUESTS)
        .map(|index| {
            let store = Arc::clone(&store);
            let embedder = Arc::clone(&embedder);
            let dispatcher = Arc::clone(&dispatcher);
            std::thread::spawn(move || {
                // Each worker builds its OWN borrowed ServerContext from the shared Arc deps (mirroring
                // serve.rs), then dispatches — exactly the per-connection model.
                let ctx = ServerContext {
                    store: &*store,
                    embedder: &*embedder,
                };
                let request = SessionRequest {
                    id: Some(json!(index)),
                    command: "fetch".to_owned(),
                    args: json!({}),
                };
                let response = dispatcher.dispatch(&ctx, &request);
                response.result().expect("a result")["snapshot_id"]
                    .as_str()
                    .expect("snapshot id")
                    .to_owned()
            })
        })
        .collect();

    let ids: std::collections::HashSet<String> =
        handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(
        opened.load(Ordering::SeqCst),
        REQUESTS,
        "each request opened exactly one snapshot"
    );
    assert_eq!(
        ids.len(),
        REQUESTS,
        "every request received its OWN (distinct) snapshot id"
    );
}

// ---- work/09 P6: thin client over TCP (the single-host "two-host acceptance" slice) ----------------

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

/// work/09 P6 acceptance (single-host topology): a query-ready site is served over a loopback TCP port,
/// and the THIN CLIENT (`jurisearch-client`, a structurally-separate artifact) queries it BY URL,
/// rendering byte-identically to the in-process site path — which the 4B e2e already proved equals the
/// one-shot CLI. So: thin client over TCP == in-process site == one-shot CLI. (The producer→syncd
/// catch-up that populates the site is covered by P5's daemon acceptance; this proves the serve→client
/// →render leg + the versioned protocol over a real socket.)
#[test]
fn the_thin_client_queries_the_site_over_tcp_with_one_shot_render_parity()
-> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-p6-site.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    ready_site(&postgres)?;
    let backend = ManagedPostgresBackend::new(
        &postgres,
        DEFAULT_READ_ROLE,
        DEFAULT_WRITER_ROLE,
        DEFAULT_OWNER_ROLE,
    );
    let embedder = StubEmbedder {
        fingerprint: "fp".to_owned(),
        dimension: 1024,
    };
    // The baseline byte-shape: the in-process site dispatch render (== one-shot, per the 4B e2e).
    let in_process_fetch = round_trip(
        &backend,
        &embedder,
        &site_line("fetch", json!({"ids": ["cass:SITE"]})),
    )?;
    let expected_fetch_render =
        jurisearch_render::render_session_response(&in_process_fetch).expect("render");

    // Serve the site over a loopback TCP port (the LAN-exposable surface, bound to loopback here). The
    // server thread OWNS the read-role store + embedder + dispatcher and serves a fixed number of
    // connections (the thin client opens one per request).
    let store = backend.read_handle()?;
    let dispatcher = build_dispatcher(4);
    let listener = TcpListener::bind("127.0.0.1:0").map_err(StorageError::Io)?;
    let addr = listener.local_addr().map_err(StorageError::Io)?;
    let server = std::thread::spawn(move || {
        let ctx = ServerContext {
            store: &store,
            embedder: &embedder,
        };
        for _ in 0..2 {
            match listener.accept() {
                Ok((stream, _)) => {
                    let Ok(reader_half) = stream.try_clone() else {
                        continue;
                    };
                    let _ = serve_site_connection(
                        BufReader::new(reader_half),
                        stream,
                        &dispatcher,
                        &ctx,
                    );
                }
                Err(_) => break,
            }
        }
    });

    let endpoint = jurisearch_client::parse_endpoint(&format!("tcp://{addr}"))
        .expect("the loopback tcp URL parses");

    // fetch over the wire: byte-identical render to the in-process site path (== one-shot CLI).
    let fetch = jurisearch_client::send_request(
        &endpoint,
        &SessionRequest {
            id: Some(json!("client")),
            command: "fetch".to_owned(),
            args: json!({"ids": ["cass:SITE"]}),
        },
    )
    .expect("the thin client fetch succeeds over TCP");
    assert!(fetch.is_ok(), "thin-client fetch ok: {fetch:?}");
    let client_fetch_render = jurisearch_render::render_session_response(&fetch).expect("render");
    assert_eq!(
        client_fetch_render, expected_fetch_render,
        "the thin client renders byte-identically to the in-process site path (== one-shot CLI)"
    );
    assert!(client_fetch_render.contains("cass:SITE"));

    // status over the wire: the health surface answers the thin client too.
    let status = jurisearch_client::send_request(
        &endpoint,
        &SessionRequest {
            id: Some(json!("client")),
            command: "status".to_owned(),
            args: json!({}),
        },
    )
    .expect("the thin client status succeeds over TCP");
    assert!(status.is_ok());
    assert_eq!(
        status.result().expect("status")["service"].as_str(),
        Some("jurisearch-site")
    );

    server.join().expect("server thread");
    Ok(())
}

/// work/09 P6: the thin client REJECTS an old/incompatible server that replies with a BARE (unversioned)
/// response — protocol skew fails loudly, never silently accepted. No PG needed (a fake server).
#[test]
fn the_thin_client_rejects_an_old_servers_unversioned_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let server = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            // Read the (versioned) request line, then reply BARE — like a pre-P6 server.
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let bare = jurisearch_transport::encode_bare_response_line(&SessionResponse::ok(
                Some(json!("client")),
                json!({}),
            ));
            let _ = stream.write_all(bare.as_bytes());
            let _ = stream.flush();
        }
    });
    let endpoint = jurisearch_client::parse_endpoint(&format!("tcp://{addr}")).expect("url");
    let result = jurisearch_client::send_request(
        &endpoint,
        &SessionRequest {
            id: Some(json!("client")),
            command: "status".to_owned(),
            args: json!({}),
        },
    );
    server.join().expect("server thread");
    assert!(
        matches!(result, Err(jurisearch_client::ClientError::ProtocolSkew(_))),
        "a bare reply from an old server must be a loud protocol-skew error: {result:?}"
    );
}

// ---- global-review BLOCKER 1: MULTI-CORPUS site serving (aggregate readiness) -----------------------

fn corpus_stamps(baseline_id: &'static str) -> ActivationStamps<'static> {
    ActivationStamps {
        sequence: 1,
        baseline_id,
        schema_version: 24,
        embedding_fingerprint: "fp",
        builder_versions: &serde_json::Value::Null,
        last_package_id: None,
        last_package_digest: None,
    }
}

/// Stand up a TWO-corpus query-ready site: `core` via the real public→generation→activate path, and a
/// second corpus `alt` built by hand into its own generation schema (the contract maps every known
/// source to `core`, so a 2nd corpus must be hand-built — as in the P3C fan-out harness) then activated
/// through the SAME visibility path. That activation grants both generations to the read role,
/// regenerates the union views to span both, and writes the AGGREGATE readiness stamp (core + alt).
fn ready_two_corpus_site(postgres: &ManagedPostgres) -> Result<(), StorageError> {
    {
        let mut superuser = postgres.client()?;
        provision_roles(&mut superuser, &RoleSpec::default(), &postgres.database)?;
    }
    let visibility = ActivationReadVisibility {
        read_role: DEFAULT_READ_ROLE,
        view_owner_role: DEFAULT_OWNER_ROLE,
    };
    let embedding = (0..1024).map(|_| "0.01").collect::<Vec<_>>().join(",");

    // --- core (real path) ---
    postgres.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:CORE','cass','decision','cass:CORE','Cass','Arret core','le corps core', \
           '2024-01-01','sha256:core','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:CORE#0','cass:CORE',0,'le corps core','ctx le corps core','sha256:cc','c1','fp');",
    )?;
    postgres.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:CORE#0','fp','[{embedding}]'::vector,'m',1024);"
    ))?;
    let core_gen = create_generation_from_public(postgres, "core", 1, Some("core-2c-g0001"))?;
    activate_generation_with_guard_and_visibility(
        postgres,
        "core",
        &core_gen,
        &corpus_stamps("core-2c-g0001"),
        CursorGuard::FirstBaseline,
        &[],
        &visibility,
    )?;

    // --- alt (hand-built generation, then activated through the visibility path) ---
    let alt_gen = {
        let mut client = postgres.client()?;
        create_generation_schema(&mut client, "alt", 1, Some("alt-2c-g0001"))?
    };
    let alt_schema = generation_schema("alt", 1);
    postgres.execute_sql(&format!(
        "INSERT INTO {alt_schema}.documents (document_id, source, kind, source_uid, citation, title, \
           body, valid_from, source_payload_hash, canonical_json) \
         VALUES ('alt:ALT','cass','decision','alt:ALT','Alt','Arret alt','le corps alt', \
           '2024-01-01','sha256:alt','{{}}'); \
         INSERT INTO {alt_schema}.chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('alt:ALT#0','alt:ALT',0,'le corps alt','ctx le corps alt','sha256:ac','c1','fp');"
    ))?;
    postgres.execute_sql(&format!(
        "INSERT INTO {alt_schema}.chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, \
           dimension) VALUES ('alt:ALT#0','fp','[{embedding}]'::vector,'m',1024);"
    ))?;
    activate_generation_with_guard_and_visibility(
        postgres,
        "alt",
        &alt_gen,
        &corpus_stamps("alt-2c-g0001"),
        CursorGuard::FirstBaseline,
        &[],
        &visibility,
    )?;
    Ok(())
}

#[test]
fn the_site_serves_a_multi_corpus_topology_through_the_read_role() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-mc-site.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    ready_two_corpus_site(&postgres)?;
    let backend = ManagedPostgresBackend::new(
        &postgres,
        DEFAULT_READ_ROLE,
        DEFAULT_WRITER_ROLE,
        DEFAULT_OWNER_ROLE,
    );
    let embedder = StubEmbedder {
        fingerprint: "fp".to_owned(),
        dimension: 1024,
    };

    // health: the aggregate topology is ready (NOT "deferred"), with both corpora.
    let health = round_trip(&backend, &embedder, &site_line("status", json!({})))?;
    let result = health.result().expect("health");
    assert_eq!(
        result["multi_corpus_readiness"].as_str(),
        Some("multi_corpus"),
        "a 2-corpus site is served, not deferred: {result}"
    );
    assert_eq!(result["readiness"]["ready"].as_bool(), Some(true));
    assert_eq!(result["active_corpora"].as_array().map(Vec::len), Some(2));

    // fetch by id resolves through the UNION views across BOTH corpora (no longer gate-blocked).
    for id in ["cass:CORE", "alt:ALT"] {
        let fetched = round_trip(
            &backend,
            &embedder,
            &site_line("fetch", json!({"ids": [id]})),
        )?;
        assert!(fetched.is_ok(), "multi-corpus fetch of {id}: {fetched:?}");
        assert_eq!(
            fetched.result().expect("result")["documents"][0]["document_id"].as_str(),
            Some(id)
        );
    }

    // bm25 search reaches the PHYSICAL fan-out (>1 corpus) and fuses results from both generations.
    let search = round_trip(
        &backend,
        &embedder,
        &site_line(
            "search",
            json!({"query": "corps", "mode": "bm25", "kind": "decision"}),
        ),
    )?;
    assert!(search.is_ok(), "multi-corpus search: {search:?}");
    let docs: Vec<&str> = search.result().expect("search")["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .filter_map(|candidate| candidate["document_id"].as_str())
        .collect();
    assert!(
        docs.contains(&"cass:CORE") && docs.contains(&"alt:ALT"),
        "search fans out over BOTH corpora: {docs:?}"
    );
    Ok(())
}
