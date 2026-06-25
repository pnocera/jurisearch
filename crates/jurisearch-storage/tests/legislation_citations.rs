mod common;

use common::discover_pg_config;
use jurisearch_storage::{
    legislation_citations::{
        InsertCitationOccurrence, finalize_citation_occurrence_counts,
        insert_citation_occurrence_with_client, legislation_citations_coverage_json,
        load_archived_decisions_with_visa_json, load_pending_citation_resolutions_json,
        update_citation_resolution_with_client, upsert_citation_resolution_pending_with_client,
    },
    official_api_archive::{InsertOfficialApiResponse, insert_official_api_response_with_client},
    runtime::{ManagedPostgres, StorageError},
};
use serde_json::json;

#[test]
fn legislation_citation_collect_and_resolve_round_trip() -> Result<(), StorageError> {
    // v17: extract from archived /decision visa -> occurrences + deduped resolutions -> resolve via
    // Legifrance (recorded). Two decisions citing the SAME article must dedup to ONE resolution.
    let Some(pg_config) = discover_pg_config("legislation citations")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-legi-cites.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    // Two cass decisions, each with an archived /decision response that cites art. 609 CPC.
    let visa = json!([{"title": "Article 609 du code de procédure civile."}]);
    for doc in ["cass:DEC1", "cass:DEC2"] {
        postgres.execute_sql(&format!(
            "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
               valid_from, source_payload_hash, canonical_json) \
             VALUES ('{doc}','cass','decision','{doc}','Cass','Arret','corps','2024-01-01', \
               'sha256:{doc}','{{}}');"
        ))?;
        let response = json!({"id": doc, "visa": visa});
        let response_id = insert_official_api_response_with_client(
            &mut client,
            &InsertOfficialApiResponse {
                provider: "judilibre",
                api_environment: "production",
                endpoint: "/cassation/judilibre/v1.0/decision",
                http_method: "GET",
                subject_document_id: Some(doc),
                subject_source_uid: Some(doc),
                provider_object_id: Some(doc),
                citation_key: None,
                request_fingerprint: "id",
                request_url: None,
                request_json: &json!({}),
                request_body: None,
                outcome: "ok",
                http_status: Some(200),
                response_body: &response.to_string(),
                response_json: Some(&response),
                response_body_sha256: "sha256:x",
                error: None,
                run_id: None,
                code_version: Some("test"),
            },
        )?;

        // Record the occurrence + deduped pending resolution (citation_key identical for both decisions).
        let key = "legi-cite:art609cpc";
        insert_citation_occurrence_with_client(
            &mut client,
            &InsertCitationOccurrence {
                decision_document_id: doc,
                decision_source_uid: doc,
                source_response_id: response_id,
                visa_index: 0,
                citation_key: key,
                article_number_raw: Some("609"),
                article_number_norm: "609",
                code_name_raw: Some("code de procédure civile"),
                code_name_norm: "code de procédure civile",
                canonical_query: "609 code de procédure civile",
                legifrance_url: None,
                raw_title: "Article 609 du code de procédure civile.",
                extraction_method: "visa_title_regex",
            },
        )?;
        upsert_citation_resolution_pending_with_client(
            &mut client,
            key,
            "609",
            "code de procédure civile",
            "609 code de procédure civile",
        )?;
    }

    // load_archived_decisions_with_visa_json returns both decisions with their visa.
    let archived: serde_json::Value = serde_json::from_str(
        &load_archived_decisions_with_visa_json(&postgres, None, 100)?,
    )
    .expect("archived JSON");
    assert_eq!(
        archived["decisions"].as_array().expect("decisions").len(),
        2
    );

    finalize_citation_occurrence_counts(&postgres)?;

    // Deduped: 2 occurrences, 1 unique resolution with occurrence_count=2.
    let coverage: serde_json::Value =
        serde_json::from_str(&legislation_citations_coverage_json(&postgres)?).expect("coverage");
    assert_eq!(coverage["occurrences"], 2);
    assert_eq!(coverage["unique_citations"], 1);

    // One pending resolution; after recording an ok Legifrance result it is no longer pending.
    let pending: serde_json::Value = serde_json::from_str(&load_pending_citation_resolutions_json(
        &postgres, None, false, 100,
    )?)
    .expect("pending JSON");
    let citations = pending["citations"].as_array().expect("citations");
    assert_eq!(citations.len(), 1);
    assert_eq!(citations[0]["citation_key"], "legi-cite:art609cpc");

    update_citation_resolution_with_client(
        &mut client,
        "legi-cite:art609cpc",
        "ok",
        None,
        Some(
            "legifrance-search:sha256:0000000000000000000000000000000000000000000000000000000000000000",
        ),
        None,
    )?;
    let still_pending: serde_json::Value = serde_json::from_str(
        &load_pending_citation_resolutions_json(&postgres, None, false, 100)?,
    )
    .expect("pending JSON");
    assert_eq!(
        still_pending["citations"].as_array().expect("c").len(),
        0,
        "resolved citation is no longer pending"
    );
    Ok(())
}
