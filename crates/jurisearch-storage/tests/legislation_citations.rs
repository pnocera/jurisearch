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
                corpus: None,
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
            None,
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
            None,
        )?;
        upsert_citation_resolution_pending_with_client(
            &mut client,
            key,
            "609",
            "code de procédure civile",
            "609 code de procédure civile",
            doc,
            None,
        )?;
    }

    // Corpus attribution (P0, BLOCKER-2 fix): a Legifrance archive row has NO subject document, only
    // the deduped citation_key — it must still resolve to exactly one corpus via the citation's
    // occurrences (the document-derived path).
    let legifrance_response = json!({"results": []});
    insert_official_api_response_with_client(
        &mut client,
        &InsertOfficialApiResponse {
            provider: "legifrance",
            api_environment: "production",
            endpoint: "/search/code",
            http_method: "POST",
            subject_document_id: None,
            subject_source_uid: None,
            provider_object_id: None,
            citation_key: Some("legi-cite:art609cpc"),
            // No subject document: the Legifrance lookup belongs to its resolution's corpus.
            corpus: Some("core"),
            request_fingerprint: "legifrance-search",
            request_url: None,
            request_json: &json!({}),
            request_body: None,
            outcome: "ok",
            http_status: Some(200),
            response_body: &legifrance_response.to_string(),
            response_json: Some(&legifrance_response),
            response_body_sha256: "sha256:y",
            error: None,
            run_id: None,
            code_version: Some("test"),
        },
        None,
    )?;

    // Every archived response (2 judilibre /decision via subject_document_id + 1 legifrance via
    // citation_key) and the deduped resolution all attribute to exactly one corpus = 'core'.
    let archive_corpora = client
        .query(
            "SELECT DISTINCT corpus FROM official_api_responses ORDER BY corpus;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    assert_eq!(
        archive_corpora.len(),
        1,
        "every official_api_responses row resolves to exactly one corpus"
    );
    assert_eq!(archive_corpora[0].get::<_, String>("corpus"), "core");

    let resolution_corpus: String = client
        .query_one(
            "SELECT corpus FROM legislation_citation_resolutions WHERE citation_key = $1;",
            &[&"legi-cite:art609cpc"],
        )
        .map_err(StorageError::PostgresClient)?
        .get("corpus");
    assert_eq!(
        resolution_corpus, "core",
        "the deduped resolution resolves to exactly one corpus"
    );

    // load_archived_decisions_with_visa_json returns both decisions with their visa.
    let archived: serde_json::Value = serde_json::from_str(
        &load_archived_decisions_with_visa_json(&postgres, None, 100)?,
    )
    .expect("archived JSON");
    assert_eq!(
        archived["decisions"].as_array().expect("decisions").len(),
        2
    );

    finalize_citation_occurrence_counts(&postgres, None)?;

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
        "core",
        "legi-cite:art609cpc",
        "ok",
        None,
        Some(
            "legifrance-search:sha256:0000000000000000000000000000000000000000000000000000000000000000",
        ),
        None,
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

#[test]
fn per_corpus_citation_resolutions_do_not_collide() -> Result<(), StorageError> {
    // r2 BLOCKER fix: resolutions are keyed (corpus, citation_key). The SAME legislation article
    // cited from two corpora gets an INDEPENDENT resolution per corpus (per-corpus replicated data,
    // INV-4) — never a cross-corpus collision and never silent attribution to the first corpus seen.
    // The old global `citation_key` PK would have rejected the second row as a duplicate.
    let Some(pg_config) = discover_pg_config("per-corpus resolutions")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-percorpus-cites.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    postgres.execute_sql(
        "INSERT INTO legislation_citation_resolutions \
             (corpus, citation_key, article_number_norm, code_name_norm, canonical_query) \
         VALUES \
             ('core','legi-cite:shared','609','code de procédure civile','609 cpc'), \
             ('inpi','legi-cite:shared','609','code de procédure civile','609 cpc');",
    )?;

    let corpora = postgres.execute_sql(
        "SELECT corpus FROM legislation_citation_resolutions \
         WHERE citation_key = 'legi-cite:shared' ORDER BY corpus;",
    )?;
    assert_eq!(
        corpora.split_whitespace().collect::<Vec<_>>(),
        vec!["core", "inpi"],
        "the same citation_key resolves to one independent resolution per corpus"
    );

    // The composite-cursor pending pager returns both corpora's pending rows, each carrying its corpus.
    let pending: serde_json::Value = serde_json::from_str(&load_pending_citation_resolutions_json(
        &postgres, None, false, 100,
    )?)
    .expect("pending JSON");
    let citations = pending["citations"].as_array().expect("citations");
    assert_eq!(
        citations.len(),
        2,
        "both per-corpus resolutions are pending"
    );
    let corpora: Vec<&str> = citations
        .iter()
        .map(|c| c["corpus"].as_str().expect("corpus present"))
        .collect();
    assert_eq!(corpora, vec!["core", "inpi"]);
    Ok(())
}
