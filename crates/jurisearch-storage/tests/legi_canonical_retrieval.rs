//! Ignored end-to-end smoke from official LEGI XML into the storage retrieval path.
//!
//! Run with:
//! `JURISEARCH_LEGI_ARCHIVE=/path/to/Freemium_legi_global_*.tar.gz cargo test -p jurisearch-storage --test legi_canonical_retrieval -- --ignored --nocapture`.

mod common;

use std::{
    collections::BTreeSet,
    env,
    error::Error,
    path::{Path, PathBuf},
};

use common::{discover_pg_config, vector_literal};
use jurisearch_ingest::{
    archive::{ArchiveVisit, DEFAULT_MEMBER_BYTE_LIMIT, for_each_xml_member_until},
    legi::{CanonicalDocument, ParsedLegiXml, parse_legi_member},
};
use jurisearch_storage::{
    projection::{ChunkEmbeddingInsert, insert_chunk_embeddings, insert_legi_documents},
    retrieval::{
        DecisionFilters, FetchDocumentsQuery, GroupBy, HybridCandidateQuery, RetrievalMode,
        RetrievalOptions, fetch_documents_json, hybrid_candidates_json,
    },
    runtime::ManagedPostgres,
};

const DEFAULT_LEGI_ARCHIVE: &str =
    "/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000.tar.gz";
const EMBEDDING_FINGERPRINT: &str = "bge-m3:1024:normalize:true";
const ARTICLE_SAMPLE_TARGET: usize = 12;
const MAX_VISITED_MEMBERS: usize = 5_000;

#[test]
#[ignore = "requires a local official LEGI tar.gz dump plus pg_search/pgvector"]
fn real_legi_canonical_subset_is_searchable_and_fetchable() -> Result<(), Box<dyn Error>> {
    let archive_path = archive_path();
    if !archive_path.exists() {
        eprintln!(
            "skipping LEGI canonical retrieval smoke because `{}` does not exist",
            archive_path.display()
        );
        return Ok(());
    }
    let Some(pg_config) = discover_pg_config("LEGI canonical retrieval smoke")? else {
        return Ok(());
    };

    let sample = collect_article_sample(&archive_path)?;
    let target = choose_target_document(&sample.documents)?;
    let query_text = query_terms_from_body(&target.body);
    assert!(
        !query_text.is_empty(),
        "target document did not expose searchable query terms: {}",
        target.document_id
    );

    let root = tempfile::Builder::new()
        .prefix("jurisearch-legi-canonical-pg.")
        .tempdir()?;
    let target_vector = vector_literal(0);
    let decoy_vector = vector_literal(1);
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    let insert_report =
        insert_legi_documents(&postgres, &sample.documents, Some(EMBEDDING_FINGERPRINT))?;
    assert_eq!(insert_report.documents, sample.documents.len());
    assert_eq!(
        insert_report.chunks,
        sample
            .documents
            .iter()
            .map(|document| document.chunks.len())
            .sum::<usize>()
    );
    assert!(
        insert_report.publisher_edges > 0,
        "expected real LEGI sample to store publisher graph-edge candidates"
    );

    let embedding_literals = sample
        .documents
        .iter()
        .flat_map(|document| {
            document.chunks.iter().map(|chunk| {
                if chunk.document_id == target.document_id {
                    target_vector.as_str()
                } else {
                    decoy_vector.as_str()
                }
            })
        })
        .collect::<Vec<_>>();
    let embeddings = sample
        .documents
        .iter()
        .flat_map(|document| document.chunks.iter())
        .zip(embedding_literals.iter().copied())
        .map(|(chunk, embedding_literal)| ChunkEmbeddingInsert {
            chunk_id: chunk.chunk_id.as_str(),
            embedding_fingerprint: EMBEDDING_FINGERPRINT,
            embedding_literal,
            model: "bge-m3",
            dimension: 1024,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        insert_chunk_embeddings(&postgres, &embeddings)?,
        embeddings.len()
    );

    let lexical_hit = postgres.execute_sql(&format!(
        "SELECT c.chunk_id \
         FROM chunks c \
         JOIN documents d ON d.document_id = c.document_id \
         WHERE c.contextualized_body @@@ {} \
           AND (d.valid_from IS NULL OR d.valid_from <= {}::date) \
           AND (d.valid_to IS NULL OR d.valid_to > {}::date) \
         ORDER BY paradedb.score(c.chunk_id) DESC, c.chunk_id \
         LIMIT 20;",
        sql_literal(&query_text),
        sql_literal(&target.valid_from),
        sql_literal(&target.valid_from)
    ))?;
    assert!(
        lexical_hit
            .lines()
            .any(|chunk_id| chunk_id == target.chunks[0].chunk_id),
        "target chunk was not in lexical results for query `{query_text}`; got {lexical_hit}"
    );

    let response = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: &query_text,
            query_embedding: Some(&target_vector),
            embedding_fingerprint: Some(EMBEDDING_FINGERPRINT),
            retrieval_mode: RetrievalMode::Hybrid,
            options: RetrievalOptions::default(),
            group_by: GroupBy::Chunk,
            after_cursor: None,
            as_of: &target.valid_from,
            kind_filter: Some("article"),
            project_authority: false,
            decision_filters: DecisionFilters::default(),
            lexical_limit: 20,
            dense_limit: 20,
            limit: 5,
        },
    )?;
    let response: serde_json::Value = serde_json::from_str(&response)?;
    let top = &response["candidates"][0];
    assert_eq!(top["chunk_id"], target.chunks[0].chunk_id);
    assert_eq!(top["document_id"], target.document_id);
    assert_eq!(top["validity"]["from"], target.valid_from);
    assert_eq!(top["source_url"].as_str(), target.source_url.as_deref());
    assert!(
        top["snippet"]
            .as_str()
            .is_some_and(|snippet| !snippet.is_empty())
    );
    assert!(top["scores"]["rrf"].as_f64().is_some());
    assert!(
        top["cursor"]
            .as_str()
            .is_some_and(|cursor| !cursor.is_empty())
    );

    let before_validity = date_in_previous_year(&target.valid_from);
    let before_response = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: &query_text,
            query_embedding: Some(&target_vector),
            embedding_fingerprint: Some(EMBEDDING_FINGERPRINT),
            retrieval_mode: RetrievalMode::Hybrid,
            options: RetrievalOptions::default(),
            group_by: GroupBy::Chunk,
            after_cursor: None,
            as_of: &before_validity,
            kind_filter: Some("article"),
            project_authority: false,
            decision_filters: DecisionFilters::default(),
            lexical_limit: 20,
            dense_limit: 20,
            limit: 10,
        },
    )?;
    let before_response: serde_json::Value = serde_json::from_str(&before_response)?;
    assert!(
        before_response["candidates"]
            .as_array()
            .expect("candidates is an array")
            .iter()
            .all(|candidate| candidate["document_id"] != target.document_id),
        "future LEGI version leaked into as-of={before_validity}: {before_response}"
    );

    let fetch = fetch_documents_json(
        &postgres,
        &FetchDocumentsQuery {
            document_ids: &[target.document_id.as_str()],
        },
    )?;
    let fetch: serde_json::Value = serde_json::from_str(&fetch)?;
    let fetched = &fetch["documents"][0];
    assert_eq!(fetched["document_id"], target.document_id);
    assert_eq!(fetched["body"], target.body);
    assert_eq!(fetched["chunks"][0]["body"], target.chunks[0].body);
    assert_eq!(
        fetched["chunks"][0]["embedding_fingerprint"],
        EMBEDDING_FINGERPRINT
    );

    let stored_edges = postgres.execute_sql("SELECT count(*)::text FROM graph_edges;")?;
    assert_eq!(stored_edges, insert_report.publisher_edges.to_string());
    let stored_member_path = postgres.execute_sql(&format!(
        "SELECT canonical_json->>'source_member_path' \
         FROM documents \
         WHERE document_id = {};",
        sql_literal(&target.document_id)
    ))?;
    assert_eq!(
        stored_member_path,
        target.source_member_path.as_deref().unwrap_or_default()
    );

    eprintln!(
        "stored {} LEGI articles, {} chunks, {} publisher edges from `{}` after visiting {} XML members; query=`{}` target={}",
        insert_report.documents,
        insert_report.chunks,
        insert_report.publisher_edges,
        archive_path.display(),
        sample.visited_xml,
        query_text,
        target.document_id
    );
    Ok(())
}

struct ArticleSample {
    documents: Vec<CanonicalDocument>,
    visited_xml: usize,
}

fn collect_article_sample(archive_path: &Path) -> Result<ArticleSample, Box<dyn Error>> {
    let mut documents = Vec::new();
    let mut publisher_edges = 0usize;
    let mut unsupported_roots = BTreeSet::new();
    let mut parse_errors = Vec::new();
    let visited_xml =
        for_each_xml_member_until(archive_path, DEFAULT_MEMBER_BYTE_LIMIT, |member| {
            let member_path = member.member_path.clone();
            match parse_legi_member(&member) {
                Ok(ParsedLegiXml::Article(document)) => {
                    publisher_edges += document.publisher_edges.len();
                    documents.push(*document);
                }
                Ok(ParsedLegiXml::UnsupportedRoot { root }) => {
                    unsupported_roots.insert(root);
                }
                Ok(
                    ParsedLegiXml::TextVersion(_)
                    | ParsedLegiXml::SectionTa(_)
                    | ParsedLegiXml::TextStruct(_),
                ) => {}
                Err(error) => {
                    parse_errors.push(format!("{member_path}: {error}"));
                }
            }

            Ok(
                if (documents.len() >= ARTICLE_SAMPLE_TARGET && publisher_edges > 0)
                    || documents.len() >= ARTICLE_SAMPLE_TARGET * 2
                {
                    ArchiveVisit::Stop
                } else {
                    ArchiveVisit::Continue
                },
            )
        })?;

    assert!(
        parse_errors.is_empty(),
        "unexpected parse errors in official LEGI sample:\n{}",
        parse_errors.join("\n")
    );
    assert!(
        documents.len() >= ARTICLE_SAMPLE_TARGET,
        "expected at least {ARTICLE_SAMPLE_TARGET} ARTICLE documents, got {}; unsupported roots: {unsupported_roots:?}",
        documents.len()
    );
    assert!(
        publisher_edges > 0,
        "expected real LEGI article sample to include publisher edges"
    );
    assert!(
        visited_xml <= MAX_VISITED_MEMBERS,
        "test should stop after a small sample, visited {visited_xml} XML members"
    );
    Ok(ArticleSample {
        documents,
        visited_xml,
    })
}

fn choose_target_document(
    documents: &[CanonicalDocument],
) -> Result<&CanonicalDocument, Box<dyn Error>> {
    documents
        .iter()
        .find(|document| {
            document.chunks.len() == 1
                && document.valid_from[0..4].parse::<i32>().unwrap_or_default() > 1
                && !query_terms_from_body(&document.body).is_empty()
        })
        .ok_or_else(|| "no searchable LEGI document in sample".into())
}

fn query_terms_from_body(body: &str) -> String {
    let stopwords = [
        "article", "articles", "cette", "leurs", "pour", "dans", "avec", "sont", "etre", "ainsi",
        "toute", "toutes",
    ];
    let mut terms = Vec::<String>::new();
    for token in body
        .split(|character: char| !character.is_alphabetic())
        .map(str::trim)
        .filter(|token| token.chars().count() >= 6)
    {
        let term = token.to_lowercase();
        if stopwords.contains(&term.as_str()) || terms.contains(&term) {
            continue;
        }
        terms.push(term);
        if terms.len() == 3 {
            break;
        }
    }
    terms.join(" ")
}

fn date_in_previous_year(date: &str) -> String {
    let year = date[0..4].parse::<i32>().unwrap_or(1);
    format!("{:04}-01-01", year.saturating_sub(1).max(1))
}

fn archive_path() -> PathBuf {
    env::var_os("JURISEARCH_LEGI_ARCHIVE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LEGI_ARCHIVE))
}

fn sql_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
