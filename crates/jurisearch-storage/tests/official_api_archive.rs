mod common;

use common::discover_pg_config;
use jurisearch_storage::{
    official_api_archive::{InsertOfficialApiResponse, insert_official_api_response_with_client},
    runtime::{ManagedPostgres, StorageError},
};
use serde_json::json;

#[test]
fn official_api_responses_archive_round_trip() -> Result<(), StorageError> {
    // v16: the durable, append-only archive persists raw API exchanges (success + error + local) with
    // body + parsed json + sha256, independent of the decision_zones cache.
    let Some(pg_config) = discover_pg_config("official api archive")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-api-archive.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // Migrations reached the current head (v18 added corpus attribution to the archive).
    let schema = postgres.execute_sql(
        "SELECT (value->>'schema_version') FROM index_manifest WHERE key = 'schema';",
    )?;
    assert_eq!(schema.trim(), "20");

    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    // The archive's corpus is derived from its subject document (P0 attribution), so the subject
    // decisions must exist for the NOT NULL `corpus` to resolve (else the insert correctly fails loud).
    for doc in ["cass:JURITEXT0001", "cass:NOPOURVOI"] {
        postgres.execute_sql(&format!(
            "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
               valid_from, source_payload_hash, canonical_json) \
             VALUES ('{doc}','cass','decision','{doc}','Cass','Arret','corps','2024-01-01', \
               'sha256:{doc}','{{}}');"
        ))?;
    }

    // A successful Judilibre /decision exchange.
    let response = json!({"id": "abc", "zones": {"motivations": []}});
    let request = json!({"params": {"id": "abc"}});
    let id = insert_official_api_response_with_client(
        &mut client,
        &InsertOfficialApiResponse {
            provider: "judilibre",
            api_environment: "production",
            endpoint: "/cassation/judilibre/v1.0/decision",
            http_method: "GET",
            subject_document_id: Some("cass:JURITEXT0001"),
            subject_source_uid: Some("cass:JURITEXT0001"),
            provider_object_id: Some("abc"),
            citation_key: None,
            corpus: None,
            request_fingerprint: "id=abc&resolve_references=false",
            request_url: Some("https://api.example/decision?id=abc"),
            request_json: &request,
            request_body: None,
            outcome: "ok",
            http_status: Some(200),
            response_body: &response.to_string(),
            response_json: Some(&response),
            response_body_sha256: "sha256:deadbeef",
            error: None,
            run_id: None,
            code_version: Some("test:0"),
        },
        None,
    )?;
    assert!(id > 0, "archive insert returns a serial response_id");

    // A 'local' unsupported accounting row (no HTTP request made).
    let empty = json!({});
    let local_id = insert_official_api_response_with_client(
        &mut client,
        &InsertOfficialApiResponse {
            provider: "local",
            api_environment: "production",
            endpoint: "judilibre:unsupported-no-pourvoi",
            http_method: "LOCAL",
            subject_document_id: Some("cass:NOPOURVOI"),
            subject_source_uid: Some("cass:NOPOURVOI"),
            provider_object_id: None,
            citation_key: None,
            corpus: None,
            request_fingerprint: "local:unsupported-no-pourvoi",
            request_url: None,
            request_json: &empty,
            request_body: None,
            outcome: "unsupported",
            http_status: None,
            response_body: "",
            response_json: None,
            response_body_sha256: "sha256:e3b0c4",
            error: Some("no parser-valid pourvoi"),
            run_id: None,
            code_version: Some("test:0"),
        },
        None,
    )?;
    assert!(local_id > id, "append-only: each insert gets a new id");

    // Read back: the parsed jsonb is queryable and the raw body is preserved.
    let zones_present = postgres.execute_sql(
        "SELECT (response_json->'zones' ? 'motivations')::text \
         FROM official_api_responses WHERE provider='judilibre';",
    )?;
    assert_eq!(zones_present.trim(), "true");
    let counts = postgres.execute_sql(
        "SELECT provider || '=' || count(*)::text \
         FROM official_api_responses GROUP BY provider ORDER BY provider;",
    )?;
    assert!(counts.contains("judilibre=1"), "got: {counts:?}");
    assert!(counts.contains("local=1"), "got: {counts:?}");

    // Append-only: re-archiving the same request inserts a SECOND row (full history).
    insert_official_api_response_with_client(
        &mut client,
        &InsertOfficialApiResponse {
            provider: "judilibre",
            api_environment: "production",
            endpoint: "/cassation/judilibre/v1.0/decision",
            http_method: "GET",
            subject_document_id: Some("cass:JURITEXT0001"),
            subject_source_uid: Some("cass:JURITEXT0001"),
            provider_object_id: Some("abc"),
            citation_key: None,
            corpus: None,
            request_fingerprint: "id=abc&resolve_references=false",
            request_url: Some("https://api.example/decision?id=abc"),
            request_json: &request,
            request_body: None,
            outcome: "ok",
            http_status: Some(200),
            response_body: &response.to_string(),
            response_json: Some(&response),
            response_body_sha256: "sha256:deadbeef",
            error: None,
            run_id: None,
            code_version: Some("test:0"),
        },
        None,
    )?;
    let judilibre_rows = postgres.execute_sql(
        "SELECT count(*)::text FROM official_api_responses WHERE provider='judilibre';",
    )?;
    assert_eq!(
        judilibre_rows.trim(),
        "2",
        "append-only history, not upsert"
    );
    Ok(())
}
