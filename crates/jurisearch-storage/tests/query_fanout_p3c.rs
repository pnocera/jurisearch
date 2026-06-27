//! work/09 P3C acceptance: multi-corpus physical-generation fan-out + RRF fusion.
//!
//! The contract currently attributes every source to ONE corpus (`core`), so a real second corpus is not
//! yet possible; this harness builds a `core` generation through the real path and a second `alt`
//! generation by hand (its own physical schema + data, registered active in `corpus_state`). A bm25
//! search then fans out over BOTH physical generations and fuses by RRF over each arm's rank, and a
//! dense search with one corpus on a different fingerprint fails closed. Skips when the harness is absent.

mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::generations::{
    ActivationStamps, activate_generation, create_generation_from_public, create_generation_schema,
    generation_schema,
};
use jurisearch_storage::query::QueryStore;
use jurisearch_storage::retrieval::{
    DecisionFilters, GroupBy, HybridCandidateQuery, RetrievalCursor, RetrievalMode,
    RetrievalOptions, hybrid_candidates_in_snapshot,
};
use jurisearch_storage::runtime::{ManagedPostgres, StorageError};
use std::collections::HashSet;

const FP: &str = "bge-m3:1024:cls:normalize=true";

fn stamps() -> ActivationStamps<'static> {
    ActivationStamps {
        sequence: 1,
        baseline_id: "core-fanout-g0001",
        schema_version: 24,
        embedding_fingerprint: FP,
        builder_versions: &serde_json::Value::Null,
        last_package_id: None,
        last_package_digest: None,
    }
}

/// One decision to seed: its `document_id` and the `contextualized_body` the bm25 probe scores (more
/// occurrences of `alpha` ⇒ a higher score ⇒ a lower rank).
type Doc<'a> = (&'a str, &'a str);

/// Activate a real `core` generation in `public` carrying `docs` (each a decision whose chunk's
/// contextualized body is bm25-indexed under [`FP`]).
fn install_core(postgres: &ManagedPostgres, docs: &[Doc<'_>]) -> Result<(), StorageError> {
    for (index, (doc, body)) in docs.iter().enumerate() {
        postgres.execute_sql(&format!(
            "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
               valid_from, source_payload_hash, canonical_json) \
             VALUES ('{doc}','cass','decision','{doc}','Cass','Arret','{body}','2024-01-01', \
               'sha256:{index}c','{{}}'); \
             INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
               source_payload_hash, chunk_builder_version, embedding_fingerprint) \
             VALUES ('{doc}#0','{doc}',0,'{body}','{body}','sha256:{index}cc','c1','{FP}');"
        ))?;
        let vector = vector_literal(3);
        postgres.execute_sql(&format!(
            "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, \
               dimension) VALUES ('{doc}#0','{FP}','{vector}'::vector,'m',1024);"
        ))?;
    }
    let generation = create_generation_from_public(postgres, "core", 1, Some("core-fanout-g0001"))?;
    activate_generation(postgres, "core", &generation, &stamps(), None)?;
    Ok(())
}

/// Build a SECOND active corpus `alt` BY HAND: its own physical generation schema (cloned tables +
/// indexes) carrying `docs` whose chunks are bm25-indexed under `fp`, registered active in
/// `corpus_state` (so a deliberately-different `fp` exercises the fail-closed preflight).
fn install_alt(postgres: &ManagedPostgres, fp: &str, docs: &[Doc<'_>]) -> Result<(), StorageError> {
    let mut client = postgres.client()?;
    create_generation_schema(&mut client, "alt", 1, Some("alt-fanout-g0001"))?;
    let schema = generation_schema("alt", 1);
    for (index, (doc, body)) in docs.iter().enumerate() {
        let vector = vector_literal(7);
        client
            .batch_execute(&format!(
                "INSERT INTO {schema}.documents (document_id, source, kind, source_uid, citation, \
                   title, body, valid_from, source_payload_hash, canonical_json) \
                 VALUES ('{doc}','cass','decision','{doc}','Alt','Arret','{body}','2024-01-01', \
                   'sha256:{index}a','{{}}'); \
                 INSERT INTO {schema}.chunks (chunk_id, document_id, chunk_index, body, \
                   contextualized_body, source_payload_hash, chunk_builder_version, \
                   embedding_fingerprint) \
                 VALUES ('{doc}#0','{doc}',0,'{body}','{body}','sha256:{index}ac','c1','{fp}'); \
                 INSERT INTO {schema}.chunk_embeddings (chunk_id, embedding_fingerprint, embedding, \
                   model, dimension) VALUES ('{doc}#0','{fp}','{vector}'::vector,'m',1024);"
            ))
            .map_err(StorageError::PostgresClient)?;
    }
    client
        .batch_execute(&format!(
            "UPDATE jurisearch_control.generation_registry SET state='active' WHERE corpus='alt'; \
             INSERT INTO jurisearch_control.corpus_state \
               (corpus, active_generation, sequence, baseline_id, schema_version, embedding_fingerprint) \
             VALUES ('alt','alt_g0001',1,'alt-fanout-g0001',24,'{fp}');"
        ))
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}

fn bm25_query<'a>(query_text: &'a str, as_of: &'a str, limit: u32) -> HybridCandidateQuery<'a> {
    HybridCandidateQuery {
        query_text,
        query_embedding: None,
        embedding_fingerprint: None,
        retrieval_mode: RetrievalMode::Bm25,
        group_by: GroupBy::Document,
        options: RetrievalOptions::default(),
        after_cursor: None,
        as_of,
        kind_filter: None,
        decision_filters: DecisionFilters::default(),
        project_authority: false,
        lexical_limit: 50,
        dense_limit: 50,
        limit,
    }
}

#[test]
fn bm25_search_fans_out_over_both_physical_generations_and_fuses() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("p3c bm25 fan-out")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-p3c-fanout.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    install_core(&postgres, &[("cass:CORE", "alpha core body")])?;
    install_alt(&postgres, FP, &[("alt:ALT", "alpha alt body")])?;

    let mut snapshot = postgres.begin_snapshot()?;
    assert_eq!(snapshot.active_corpora().len(), 2, "two active corpora");

    let response =
        hybrid_candidates_in_snapshot(&mut *snapshot, &bm25_query("alpha", "2024-06-01", 10))?;
    let parsed: serde_json::Value = serde_json::from_str(&response).map_err(StorageError::Json)?;
    let candidates = parsed["candidates"].as_array().expect("candidates array");

    // BOTH corpora contribute, each candidate tagged with its corpus, no union-view scan.
    assert_eq!(
        candidates.len(),
        2,
        "fused candidates from both arms: {parsed}"
    );
    let docs: Vec<&str> = candidates
        .iter()
        .map(|c| c["document_id"].as_str().unwrap_or_default())
        .collect();
    assert!(
        docs.contains(&"cass:CORE") && docs.contains(&"alt:ALT"),
        "both docs present: {docs:?}"
    );
    for candidate in candidates {
        assert!(
            candidate["corpus"].is_string(),
            "each candidate carries its corpus: {candidate}"
        );
        assert!(
            candidate["cursor"]
                .as_str()
                .is_some_and(|c| c.starts_with("mc:")),
            "multi-corpus candidates carry an mc: cursor: {candidate}"
        );
        // The cross-corpus RRF score replaces the within-corpus one; the local score is preserved.
        assert!(candidate["scores"]["rrf"].is_number());
        assert!(candidate["scores"]["local_rrf"].is_number());
    }
    // Each is rank 1 in its arm → identical cross score → tie broken by (corpus, id): `alt` sorts first.
    assert_eq!(candidates[0]["corpus"].as_str(), Some("alt"));
    Ok(())
}

/// Parse `mc:<group>:<score>:<corpus>:<id>` → `(score, corpus, id)` (the id may itself contain `:`).
fn parse_mc_cursor(cursor: &str) -> (String, String, String) {
    let rest = cursor.strip_prefix("mc:").expect("an mc: cursor");
    let parts: Vec<&str> = rest.splitn(4, ':').collect();
    (
        parts[1].to_owned(),
        parts[2].to_owned(),
        parts[3].to_owned(),
    )
}

#[test]
fn multi_corpus_pagination_is_stable_and_reaches_deep_cursor_ranks() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("p3c fan-out pagination")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-p3c-page.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // SIX decisions per corpus (12 total). With a page size of 2, paging to the end requires reaching
    // local arm rank 6 — far deeper than a first-page `top_k+1` fetch would reach. So if
    // `hybrid_candidates_fanout` ever stopped sizing the per-arm fetch from the cursor rank
    // (`implied_rank(cursor.score) + page + 1`) and used a fixed shallow depth, the deep-rank decisions
    // would become unreachable and the union below would be SHORT of 12.
    let core_ids: Vec<String> = (1..=6).map(|i| format!("cass:C{i}")).collect();
    let alt_ids: Vec<String> = (1..=6).map(|i| format!("alt:A{i}")).collect();
    let core_docs: Vec<Doc> = core_ids
        .iter()
        .map(|id| (id.as_str(), "alpha core"))
        .collect();
    let alt_docs: Vec<Doc> = alt_ids
        .iter()
        .map(|id| (id.as_str(), "alpha alt"))
        .collect();
    install_core(&postgres, &core_docs)?;
    install_alt(&postgres, FP, &alt_docs)?;

    let mut snapshot = postgres.begin_snapshot()?;

    // Page through the whole fused stream (page size 2), collecting every decision.
    let mut collected: Vec<String> = Vec::new();
    let mut cursor: Option<(String, String, String)> = None;
    for _page in 0..10 {
        let next_cursor: Option<(String, String, String)>;
        {
            let after = cursor
                .as_ref()
                .map(|(score, corpus, id)| RetrievalCursor::MultiCorpus {
                    score: score.as_str(),
                    corpus: corpus.as_str(),
                    id: id.as_str(),
                });
            let mut query = bm25_query("alpha", "2024-06-01", 2);
            query.after_cursor = after;
            let parsed: serde_json::Value =
                serde_json::from_str(&hybrid_candidates_in_snapshot(&mut *snapshot, &query)?)
                    .map_err(StorageError::Json)?;
            let page = parsed["candidates"].as_array().cloned().unwrap_or_default();
            if page.is_empty() {
                break;
            }
            for candidate in &page {
                collected.push(
                    candidate["document_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                );
            }
            next_cursor = if page.len() < 2 {
                None
            } else {
                page.last()
                    .and_then(|candidate| candidate["cursor"].as_str())
                    .map(parse_mc_cursor)
            };
        }
        match next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }

    // Stable + non-overlapping across arms AND across deep cursor pages: every one of the 12 decisions is
    // returned exactly once.
    let unique: HashSet<&String> = collected.iter().collect();
    assert_eq!(
        unique.len(),
        12,
        "paging reaches every decision with no overlap (cursor-aware depth): {collected:?}"
    );
    assert_eq!(
        collected.len(),
        12,
        "no decision is returned twice: {collected:?}"
    );
    Ok(())
}

#[test]
fn dense_fan_out_fails_closed_when_a_corpus_fingerprint_differs() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("p3c dense fp mismatch")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-p3c-fpmismatch.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    install_core(&postgres, &[("cass:CORE", "alpha core body")])?; // fingerprint FP
    install_alt(
        &postgres,
        "a-different-fingerprint",
        &[("alt:ALT", "alpha alt body")],
    )?; // mismatch

    let mut snapshot = postgres.begin_snapshot()?;
    let mut query = bm25_query("alpha", "2024-06-01", 10);
    query.retrieval_mode = RetrievalMode::Dense;
    query.query_embedding = Some("[1,0,0]");
    query.embedding_fingerprint = Some(FP);

    let error = hybrid_candidates_in_snapshot(&mut *snapshot, &query).expect_err(
        "a multi-corpus dense search with a mismatched corpus fingerprint must fail closed",
    );
    assert!(
        error.to_string().contains("embedding_fingerprint_mismatch"),
        "the failure is the fail-closed preflight (no partial results): {error}"
    );
    Ok(())
}
