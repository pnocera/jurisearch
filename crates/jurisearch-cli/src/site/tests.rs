//! work/09 P4 (4A) e2e: the site service end to end over an in-memory connection, through the READ-ROLE
//! store. Proves the walking skeleton: a versioned `fetch` request frames → dispatches → reads one
//! snapshot under the least-privilege read identity → returns the document; `status` returns health; an
//! unversioned frame is rejected before dispatch; a client `index_dir` is rejected. Skips cleanly when the
//! managed-PG harness is absent.

use std::io::Cursor;

use jurisearch_core::envelope::ProtocolEnvelope;
use jurisearch_core::session::{SessionRequest, SessionResponse};
use jurisearch_storage::backend::{
    DEFAULT_OWNER_ROLE, DEFAULT_READ_ROLE, DEFAULT_WRITER_ROLE, ManagedPostgresBackend, RoleSpec,
    StorageBackend, provision_roles,
};
use jurisearch_storage::generations::{
    ActivationReadVisibility, ActivationStamps, CursorGuard,
    activate_generation_with_guard_and_visibility, create_generation_from_public,
};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_transport::{encode_bare_request_line, encode_site_envelope_line};
use serde_json::json;

use super::dispatcher::ServerContext;
use super::{build_skeleton_dispatcher, serve_site_connection};

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
/// with the read role's visibility — the supported shared-server read path.
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

/// Frame one request line, run it through the site listener over the read-role store, and decode the
/// single response line.
fn round_trip(
    backend: &ManagedPostgresBackend,
    request_line: &str,
) -> Result<SessionResponse, StorageError> {
    let store = backend.read_handle()?;
    let ctx = ServerContext { store: &store };
    let dispatcher = build_skeleton_dispatcher();
    let mut output: Vec<u8> = Vec::new();
    serve_site_connection(
        Cursor::new(format!("{request_line}\n")),
        &mut output,
        &dispatcher,
        &ctx,
    )
    .map_err(StorageError::Io)?;
    let line = String::from_utf8(output).expect("utf8 response");
    let response: SessionResponse =
        serde_json::from_str(line.trim()).map_err(StorageError::Json)?;
    Ok(response)
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
fn the_site_service_serves_a_versioned_fetch_through_the_read_role() -> Result<(), StorageError> {
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

    // A versioned `fetch` returns the document through the least-privilege read identity.
    let response = round_trip(&backend, &site_line("fetch", json!({"ids": ["cass:SITE"]})))?;
    assert!(response.is_ok(), "fetch ok: {response:?}");
    assert_eq!(
        response.id(),
        Some(&json!("r1")),
        "the response correlates by id"
    );
    let documents = response.result().expect("a result")["documents"]
        .as_array()
        .expect("documents array");
    assert_eq!(documents.len(), 1);
    let document = &documents[0];
    // The FULL FetchResponse shape — not just the id — so a regression that dropped the body/citation/
    // chunks would fail (the site path returns the same builder output as the one-shot CLI).
    assert_eq!(document["document_id"].as_str(), Some("cass:SITE"));
    assert_eq!(document["citation"].as_str(), Some("Cass"));
    assert_eq!(document["title"].as_str(), Some("Arret du site"));
    assert_eq!(document["body"].as_str(), Some("le corps"));
    let chunks = document["chunks"].as_array().expect("chunks array");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0]["chunk_id"].as_str(), Some("cass:SITE#0"));
    // Render parity: the thin client / one-shot CLI renderer prints the SAME response (P6 reuses this);
    // assert it renders to the byte-identical pretty body and is non-trivial.
    let rendered = jurisearch_render::render_session_response(&response).expect("render");
    let expected = jurisearch_render::render_value_pretty(response.result().expect("result"))
        .expect("render expected");
    assert_eq!(
        rendered, expected,
        "the site response renders with one-shot parity"
    );
    assert!(rendered.contains("cass:SITE"));

    // `status` returns server-context health (active corpus topology), not the local status payload.
    let health = round_trip(&backend, &site_line("status", json!({})))?;
    assert!(health.is_ok(), "status/health ok: {health:?}");
    let result = health.result().expect("health result");
    assert_eq!(result["service"].as_str(), Some("jurisearch-site"));
    assert_eq!(result["active_corpora"][0]["corpus"].as_str(), Some("core"));
    assert_eq!(
        result["multi_corpus_readiness"].as_str(),
        Some("single_corpus")
    );

    // An UNVERSIONED frame (the local bare request shape) is rejected before dispatch.
    let bare = encode_bare_request_line(&SessionRequest {
        id: Some(json!("r2")),
        command: "fetch".to_owned(),
        args: json!({"ids": ["cass:SITE"]}),
    });
    let rejected = round_trip(&backend, bare.trim())?;
    assert!(!rejected.is_ok(), "an unversioned site frame is rejected");

    // A client-supplied `index_dir` is rejected at the boundary (never influences the server).
    let index_dir = round_trip(
        &backend,
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
    Ok(())
}
