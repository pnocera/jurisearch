mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::{
    retrieval::{
        CitationResolutionQuery, ContextDocumentsQuery, FetchDocumentsQuery, HybridCandidateQuery,
        RetrievalCursor, RetrievalMode, context_documents_json, fetch_documents_json,
        hybrid_candidates_json, resolve_legi_citation_json,
    },
    runtime::{ManagedPostgres, StorageError},
};

#[test]
fn resolve_legi_citation_pins_version_by_as_of_and_excludes_siblings() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("citation resolution")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-citation-resolution.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    // Two versions of "Décret X Article 33" (1973-1981, 1981-open) plus a sibling "Article 34".
    postgres.execute_sql(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, citation, title, body, \
            valid_from, valid_to, source_payload_hash, canonical_json) \
         VALUES \
           ('legi:ART33V1@1973-07-14', 'legi', 'article', 'ART33V1', \
            'Décret X Article 33', 'Article 33', 'v1 body', '1973-07-14', '1981-05-16', \
            'h1', '{\"official\":true}'), \
           ('legi:ART33V2@1981-05-16', 'legi', 'article', 'ART33V2', \
            'Décret X Article 33', 'Article 33', 'v2 body', '1981-05-16', NULL, \
            'h2', '{\"official\":true}'), \
           ('legi:ART34@1973-07-14', 'legi', 'article', 'ART34', \
            'Décret X Article 34', 'Article 34', 'sibling body', '1973-07-14', NULL, \
            'h3', '{\"official\":true}'), \
           ('legi:ART330@1985-01-01', 'legi', 'article', 'ART330', \
            'Décret X Article 330', 'Article 330', 'prefix sibling body', '1985-01-01', NULL, \
            'h4', '{\"official\":true}'); \
         INSERT INTO chunks \
           (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
            chunk_builder_version, embedding_fingerprint) \
         VALUES \
           ('c33v1', 'legi:ART33V1@1973-07-14', 0, 'v1', 'v1', 'h1', 'cv0', NULL), \
           ('c33v2', 'legi:ART33V2@1981-05-16', 0, 'v2', 'v2', 'h2', 'cv0', NULL), \
           ('c34', 'legi:ART34@1973-07-14', 0, 's', 's', 'h3', 'cv0', NULL), \
           ('c330', 'legi:ART330@1985-01-01', 0, 'p', 'p', 'h4', 'cv0', NULL);",
    )?;

    let resolve = |as_of: &str| -> Result<Vec<String>, StorageError> {
        let json = resolve_legi_citation_json(
            &postgres,
            &CitationResolutionQuery {
                query: "Décret X Article 33",
                article_number: "33",
                code_hint: Some("Décret X"),
                as_of,
                kind_filter: Some("article"),
                limit: 10,
            },
        )?;
        let value: serde_json::Value = serde_json::from_str(&json).expect("resolver json");
        Ok(value["candidates"]
            .as_array()
            .map(|candidates| {
                candidates
                    .iter()
                    .filter_map(|candidate| candidate["document_id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    };

    // As-of 1975: only V1 is valid; the sibling Article 34 is excluded by the article-number match.
    assert_eq!(resolve("1975-01-01")?, vec!["legi:ART33V1@1973-07-14".to_owned()]);
    // As-of 1990: V2 is the valid version. The prefix sibling "Article 330" (valid from 1985, a
    // later valid_from) must NOT be returned for "Article 33" — exact title match, not a prefix.
    assert_eq!(resolve("1990-01-01")?, vec!["legi:ART33V2@1981-05-16".to_owned()]);

    Ok(())
}

#[test]
fn migrated_schema_supports_bm25_and_vector_candidate_retrieval() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("retrieval smoke")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-retrieval-pg.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let legal_vector = vector_literal(0);
    let unrelated_vector = vector_literal(1);

    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    let bm25_index = postgres.execute_sql(
        "SELECT indexname \
         FROM pg_indexes \
         WHERE schemaname = 'public' \
           AND tablename = 'chunks' \
           AND indexname = 'chunks_bm25_idx';",
    )?;
    assert_eq!(bm25_index, "chunks_bm25_idx");

    postgres.execute_sql(&format!(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, citation, title, body, \
            valid_from, source_payload_hash, canonical_json) \
         VALUES \
           ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
            'LEGIARTI000006419320', 'Code civil article 1240', \
            'Article 1240', 'Responsabilité civile pour fautes et réparations du dommage causé par l''auteur. Créancier, procédure et arrêté.', \
            '1804-02-21', 'sha256:article-1240', '{{\"official\":true}}'), \
           ('legi:LEGIARTI000000000001@2024-01-01', 'legi', 'article', \
            'LEGIARTI000000000001', 'Code de cuisine article 1241', \
            'Article cuisine', 'Recette article 1241 de tarte aux pommes avec cannelle.', \
            '2024-01-01', 'sha256:recipe', '{{\"official\":true}}'); \
         INSERT INTO chunks \
           (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
            chunk_builder_version, embedding_fingerprint) \
         VALUES \
           ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
            'responsabilité civile fautes réparations dommage causé par l''auteur créancier procédure arrêté article 1240', \
            'Code civil > Article 1240\nresponsabilité civile fautes réparations dommage causé par l''auteur créancier procédure arrêté article 1240', \
            'sha256:article-1240', 'chunker:v0', 'bge-m3:1024:normalize:true'), \
           ('chunk:recipe:0', 'legi:LEGIARTI000000000001@2024-01-01', 0, \
            'recette article 1241 tarte pommes cannelle dessert', \
            'Code de cuisine > Article 1241\nrecette article 1241 tarte pommes cannelle dessert', \
            'sha256:recipe', 'chunker:v0', 'bge-m3:1024:normalize:true'); \
         INSERT INTO chunk_embeddings \
           (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES \
           ('chunk:1240:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024), \
           ('chunk:recipe:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024);",
        legal_vector, unrelated_vector
    ))?;

    let lexical = postgres.execute_sql(
        "SELECT chunk_id \
         FROM chunks \
         WHERE contextualized_body @@@ 'code civil' \
         ORDER BY paradedb.score(chunk_id) DESC \
         LIMIT 1;",
    )?;
    assert_eq!(lexical, "chunk:1240:0");

    let normalized_legal = postgres.execute_sql(
        "SELECT chunk_id \
         FROM chunks \
         WHERE contextualized_body @@@ 'responsabilite faute reparation' \
         ORDER BY paradedb.score(chunk_id) DESC, chunk_id \
         LIMIT 1;",
    )?;
    assert_eq!(normalized_legal, "chunk:1240:0");

    let elision_legal = postgres.execute_sql(
        "SELECT chunk_id \
         FROM chunks \
         WHERE contextualized_body @@@ 'auteur dommage' \
         ORDER BY paradedb.score(chunk_id) DESC, chunk_id \
         LIMIT 1;",
    )?;
    assert_eq!(elision_legal, "chunk:1240:0");

    let additional_accents = postgres.execute_sql(
        "SELECT chunk_id \
         FROM chunks \
         WHERE contextualized_body @@@ 'creancier procedure arrete' \
         ORDER BY paradedb.score(chunk_id) DESC, chunk_id \
         LIMIT 1;",
    )?;
    assert_eq!(additional_accents, "chunk:1240:0");

    let statutory_reference = postgres.execute_sql(
        "SELECT chunk_id \
         FROM chunks \
         WHERE contextualized_body @@@ 'article 1240' \
         ORDER BY paradedb.score(chunk_id) DESC, chunk_id \
         LIMIT 1;",
    )?;
    assert_eq!(statutory_reference, "chunk:1240:0");

    let statutory_reference_decoy = postgres.execute_sql(
        "SELECT chunk_id \
         FROM chunks \
         WHERE contextualized_body @@@ 'article 1241' \
         ORDER BY paradedb.score(chunk_id) DESC, chunk_id \
         LIMIT 1;",
    )?;
    assert_eq!(statutory_reference_decoy, "chunk:recipe:0");

    let vector = postgres.execute_sql(&format!(
        "SELECT chunk_id \
         FROM chunk_embeddings \
         ORDER BY embedding <-> '{}' \
         LIMIT 1;",
        legal_vector
    ))?;
    assert_eq!(vector, "chunk:1240:0");

    let candidates = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: "code civil",
            query_embedding: Some(&legal_vector),
            embedding_fingerprint: Some("bge-m3:1024:normalize:true"),
            retrieval_mode: RetrievalMode::Hybrid,
            after_cursor: None,
            as_of: "2024-01-01",
            kind_filter: None,
            lexical_limit: 10,
            dense_limit: 10,
            limit: 3,
        },
    )?;
    let candidates: serde_json::Value =
        serde_json::from_str(&candidates).expect("retrieval response is stable JSON");
    assert_eq!(candidates["query"], "code civil");
    assert_eq!(candidates["retrieval_mode"], "hybrid");
    assert_eq!(candidates["candidates"][0]["chunk_id"], "chunk:1240:0");
    assert_eq!(
        candidates["candidates"][0]["scores"]["lexical_rank"].as_u64(),
        Some(1)
    );
    assert_eq!(
        candidates["candidates"][0]["scores"]["dense_rank"].as_u64(),
        Some(1)
    );
    assert!(
        candidates["candidates"][0]["cursor"]
            .as_str()
            .is_some_and(|cursor| cursor.ends_with(":chunk:1240:0"))
    );

    let bm25_candidates = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: "code civil",
            query_embedding: None,
            embedding_fingerprint: None,
            retrieval_mode: RetrievalMode::Bm25,
            after_cursor: None,
            as_of: "2024-01-01",
            kind_filter: None,
            lexical_limit: 10,
            dense_limit: 10,
            limit: 3,
        },
    )?;
    let bm25_candidates: serde_json::Value =
        serde_json::from_str(&bm25_candidates).expect("BM25 response is stable JSON");
    assert_eq!(bm25_candidates["retrieval_mode"], "bm25");
    assert_eq!(bm25_candidates["candidates"][0]["chunk_id"], "chunk:1240:0");
    assert_eq!(
        bm25_candidates["candidates"][0]["scores"]["lexical_rank"].as_u64(),
        Some(1)
    );
    assert!(bm25_candidates["candidates"][0]["scores"]["dense_rank"].is_null());

    let first_article_page = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: "article",
            query_embedding: None,
            embedding_fingerprint: None,
            retrieval_mode: RetrievalMode::Bm25,
            after_cursor: None,
            as_of: "2024-01-01",
            kind_filter: None,
            lexical_limit: 10,
            dense_limit: 10,
            limit: 1,
        },
    )?;
    let first_article_page: serde_json::Value =
        serde_json::from_str(&first_article_page).expect("first page response is stable JSON");
    let first_cursor = first_article_page["candidates"][0]["cursor"]
        .as_str()
        .expect("first candidate has a cursor");
    let (cursor_score, cursor_chunk_id) = first_cursor
        .split_once(':')
        .expect("cursor is score followed by chunk id");
    let second_article_page = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: "article",
            query_embedding: None,
            embedding_fingerprint: None,
            retrieval_mode: RetrievalMode::Bm25,
            after_cursor: Some(RetrievalCursor {
                score: cursor_score,
                chunk_id: cursor_chunk_id,
            }),
            as_of: "2024-01-01",
            kind_filter: None,
            lexical_limit: 10,
            dense_limit: 10,
            limit: 1,
        },
    )?;
    let second_article_page: serde_json::Value =
        serde_json::from_str(&second_article_page).expect("second page response is stable JSON");
    assert_ne!(
        second_article_page["candidates"][0]["chunk_id"],
        first_article_page["candidates"][0]["chunk_id"]
    );

    let first_hybrid_tie_page = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: "code civil",
            query_embedding: Some(&unrelated_vector),
            embedding_fingerprint: Some("bge-m3:1024:normalize:true"),
            retrieval_mode: RetrievalMode::Hybrid,
            after_cursor: None,
            as_of: "2024-01-01",
            kind_filter: None,
            lexical_limit: 1,
            dense_limit: 1,
            limit: 1,
        },
    )?;
    let first_hybrid_tie_page: serde_json::Value =
        serde_json::from_str(&first_hybrid_tie_page).expect("hybrid tie page is stable JSON");
    assert_eq!(
        first_hybrid_tie_page["candidates"][0]["chunk_id"],
        "chunk:1240:0"
    );
    let first_hybrid_tie_cursor = first_hybrid_tie_page["candidates"][0]["cursor"]
        .as_str()
        .expect("hybrid tie candidate has a cursor");
    let (hybrid_tie_cursor_score, hybrid_tie_cursor_chunk_id) = first_hybrid_tie_cursor
        .split_once(':')
        .expect("cursor is score followed by chunk id");
    let second_hybrid_tie_page = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: "code civil",
            query_embedding: Some(&unrelated_vector),
            embedding_fingerprint: Some("bge-m3:1024:normalize:true"),
            retrieval_mode: RetrievalMode::Hybrid,
            after_cursor: Some(RetrievalCursor {
                score: hybrid_tie_cursor_score,
                chunk_id: hybrid_tie_cursor_chunk_id,
            }),
            as_of: "2024-01-01",
            kind_filter: None,
            lexical_limit: 1,
            dense_limit: 1,
            limit: 1,
        },
    )?;
    let second_hybrid_tie_page: serde_json::Value =
        serde_json::from_str(&second_hybrid_tie_page).expect("hybrid tie page is stable JSON");
    assert_eq!(
        second_hybrid_tie_page["candidates"][0]["chunk_id"],
        "chunk:recipe:0"
    );

    let dense_candidates = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: "semantic-only query",
            query_embedding: Some(&legal_vector),
            embedding_fingerprint: Some("bge-m3:1024:normalize:true"),
            retrieval_mode: RetrievalMode::Dense,
            after_cursor: None,
            as_of: "2024-01-01",
            kind_filter: None,
            lexical_limit: 10,
            dense_limit: 10,
            limit: 3,
        },
    )?;
    let dense_candidates: serde_json::Value =
        serde_json::from_str(&dense_candidates).expect("dense response is stable JSON");
    assert_eq!(dense_candidates["retrieval_mode"], "dense");
    assert_eq!(
        dense_candidates["candidates"][0]["chunk_id"],
        "chunk:1240:0"
    );
    assert!(dense_candidates["candidates"][0]["scores"]["lexical_rank"].is_null());
    assert_eq!(
        dense_candidates["candidates"][0]["scores"]["dense_rank"].as_u64(),
        Some(1)
    );

    let empty_fetch = fetch_documents_json(&postgres, &FetchDocumentsQuery { document_ids: &[] })?;
    let empty_fetch: serde_json::Value =
        serde_json::from_str(&empty_fetch).expect("empty fetch response is stable JSON");
    assert_eq!(empty_fetch["documents"].as_array().unwrap().len(), 0);

    let fetch = fetch_documents_json(
        &postgres,
        &FetchDocumentsQuery {
            document_ids: &["legi:LEGIARTI000006419320@1804-02-21"],
        },
    )?;
    let fetch: serde_json::Value =
        serde_json::from_str(&fetch).expect("fetch response is stable JSON");
    assert_eq!(
        fetch["documents"][0]["document_id"],
        "legi:LEGIARTI000006419320@1804-02-21"
    );
    assert_eq!(
        fetch["documents"][0]["chunks"][0]["embedding_fingerprint"],
        "bge-m3:1024:normalize:true"
    );

    Ok(())
}

#[test]
fn context_documents_json_reconstructs_hierarchy_and_date_filtered_siblings()
-> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("context retrieval smoke")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-context-pg.")
        .tempdir()
        .map_err(StorageError::Io)?;

    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    postgres.execute_sql(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, citation, title, body, \
            valid_from, valid_to, source_payload_hash, hierarchy_path, canonical_json) \
         VALUES \
           ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
            'LEGIARTI000006419320', 'Code civil article 1240', 'Article 1240', \
            'Responsabilite civile.', '1804-02-21', NULL, 'sha256:1240', \
            '[\"Code civil\",\"Livre III\",\"Titre IV\"]'::jsonb, \
            '{\"hierarchy_path\":[\"Code civil\",\"Livre III\",\"Titre IV\"]}'), \
           ('legi:LEGIARTI000006419321@1804-02-21', 'legi', 'article', \
            'LEGIARTI000006419321', 'Code civil article 1241', 'Article 1241', \
            'Responsabilite voisine.', '1804-02-21', NULL, 'sha256:1241', \
            '[\"Code civil\",\"Livre III\",\"Titre IV\"]'::jsonb, \
            '{\"hierarchy_path\":[\"Code civil\",\"Livre III\",\"Titre IV\"]}'), \
           ('legi:LEGIARTI000006419322@2025-01-01', 'legi', 'article', \
            'LEGIARTI000006419322', 'Code civil article futur', 'Article futur', \
            'Version future.', '2025-01-01', NULL, 'sha256:future', \
            '[\"Code civil\",\"Livre III\",\"Titre IV\"]'::jsonb, \
            '{\"hierarchy_path\":[\"Code civil\",\"Livre III\",\"Titre IV\"]}'), \
           ('legi:LEGIARTI000006419400@1804-02-21', 'legi', 'article', \
            'LEGIARTI000006419400', 'Code civil article autre', 'Article autre', \
            'Autre section.', '1804-02-21', NULL, 'sha256:other', \
            '[\"Code civil\",\"Livre III\",\"Titre V\"]'::jsonb, \
            '{\"hierarchy_path\":[\"Code civil\",\"Livre III\",\"Titre V\"]}'); \
         INSERT INTO chunks \
           (chunk_id, document_id, chunk_index, body, contextualized_body, chunking, boundary, \
            hierarchy_path, source_payload_hash, chunk_builder_version) \
         VALUES \
           ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
            'Responsabilite civile.', 'Code civil > Livre III > Titre IV > Article 1240', \
            'structural', 'article', '[\"Code civil\",\"Livre III\",\"Titre IV\"]', \
            'sha256:1240', 'chunker:v1'), \
           ('chunk:1241:0', 'legi:LEGIARTI000006419321@1804-02-21', 0, \
            'Responsabilite voisine.', 'Code civil > Livre III > Titre IV > Article 1241', \
            'structural', 'article', '[\"Code civil\",\"Livre III\",\"Titre IV\"]', \
            'sha256:1241', 'chunker:v1'), \
           ('chunk:future:0', 'legi:LEGIARTI000006419322@2025-01-01', 0, \
            'Version future.', 'Code civil > Livre III > Titre IV > Article futur', \
            'structural', 'article', '[\"Code civil\",\"Livre III\",\"Titre IV\"]', \
            'sha256:future', 'chunker:v1'), \
           ('chunk:other:0', 'legi:LEGIARTI000006419400@1804-02-21', 0, \
            'Autre section.', 'Code civil > Livre III > Titre V > Article autre', \
            'structural', 'article', '[\"Code civil\",\"Livre III\",\"Titre V\"]', \
            'sha256:other', 'chunker:v1');",
    )?;

    let context = context_documents_json(
        &postgres,
        &ContextDocumentsQuery {
            document_id: "legi:LEGIARTI000006419320@1804-02-21",
            as_of: Some("2024-01-01"),
            include_siblings: true,
        },
    )?;
    let context: serde_json::Value =
        serde_json::from_str(&context).expect("context response is stable JSON");
    assert_eq!(
        context["target"]["document_id"],
        "legi:LEGIARTI000006419320@1804-02-21"
    );
    assert_eq!(context["as_of"], "2024-01-01");
    assert_eq!(context["requested_as_of"], "2024-01-01");
    assert_eq!(context["ancestry"].as_array().unwrap().len(), 3);
    assert_eq!(context["ancestry"][1]["title"], "Livre III");
    assert_eq!(context["sibling_count"], 1);
    assert_eq!(
        context["siblings"][0]["document_id"],
        "legi:LEGIARTI000006419321@1804-02-21"
    );

    let default_date_siblings = context_documents_json(
        &postgres,
        &ContextDocumentsQuery {
            document_id: "legi:LEGIARTI000006419320@1804-02-21",
            as_of: None,
            include_siblings: true,
        },
    )?;
    let default_date_siblings: serde_json::Value =
        serde_json::from_str(&default_date_siblings).expect("context response is stable JSON");
    assert_eq!(default_date_siblings["as_of"], "1804-02-21");
    assert_eq!(default_date_siblings["sibling_count"], 1);
    assert_eq!(default_date_siblings["sibling_truncated"], false);
    assert_eq!(
        default_date_siblings["siblings"][0]["document_id"],
        "legi:LEGIARTI000006419321@1804-02-21"
    );

    postgres.execute_sql(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, citation, title, body, \
            valid_from, valid_to, source_payload_hash, hierarchy_path, canonical_json) \
         SELECT \
           'legi:generated-sibling-' || g::text || '@1804-02-21', 'legi', 'article', \
           'generated-sibling-' || g::text, 'Code civil generated sibling ' || g::text, \
           'Article S' || lpad(g::text, 2, '0'), 'Generated sibling.', \
           '1804-02-21'::date, NULL::date, 'sha256:generated-sibling-' || g::text, \
           '[\"Code civil\",\"Livre III\",\"Titre IV\"]'::jsonb, \
           '{\"hierarchy_path\":[\"Code civil\",\"Livre III\",\"Titre IV\"]}'::jsonb \
         FROM generate_series(1, 55) AS g; \
         INSERT INTO chunks \
           (chunk_id, document_id, chunk_index, body, contextualized_body, chunking, boundary, \
            hierarchy_path, source_payload_hash, chunk_builder_version) \
         SELECT \
           'chunk:generated-sibling:' || g::text || ':0', \
           'legi:generated-sibling-' || g::text || '@1804-02-21', 0, \
           'Generated sibling.', 'Code civil > Livre III > Titre IV > Article S' || g::text, \
           'structural', 'article', '[\"Code civil\",\"Livre III\",\"Titre IV\"]', \
           'sha256:generated-sibling-' || g::text, 'chunker:v1' \
         FROM generate_series(1, 55) AS g;",
    )?;

    let truncated = context_documents_json(
        &postgres,
        &ContextDocumentsQuery {
            document_id: "legi:LEGIARTI000006419320@1804-02-21",
            as_of: Some("2024-01-01"),
            include_siblings: true,
        },
    )?;
    let truncated: serde_json::Value =
        serde_json::from_str(&truncated).expect("context response is stable JSON");
    assert_eq!(truncated["sibling_count"], 56);
    assert_eq!(truncated["sibling_limit"], 50);
    assert_eq!(truncated["sibling_truncated"], true);
    assert_eq!(truncated["siblings"].as_array().unwrap().len(), 50);

    postgres.execute_sql(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, citation, title, body, \
            valid_from, source_payload_hash, hierarchy_path, canonical_json) \
         VALUES \
           ('legi:empty-path-target@1804-02-21', 'legi', 'article', 'empty-target', \
            'Empty path target', 'Article empty', 'Empty hierarchy target.', \
            '1804-02-21', 'sha256:empty-target', '[]'::jsonb, \
            '{\"hierarchy_path\":[]}'::jsonb), \
           ('legi:empty-path-other@1804-02-21', 'legi', 'article', 'empty-other', \
            'Empty path other', 'Article empty other', 'Empty hierarchy other.', \
            '1804-02-21', 'sha256:empty-other', '[]'::jsonb, \
            '{\"hierarchy_path\":[]}'::jsonb); \
         INSERT INTO chunks \
           (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
            chunk_builder_version) \
         VALUES \
           ('chunk:empty-target:0', 'legi:empty-path-target@1804-02-21', 0, \
            'Empty hierarchy target.', 'Empty hierarchy target.', \
            'sha256:empty-target', 'chunker:v1'), \
           ('chunk:empty-other:0', 'legi:empty-path-other@1804-02-21', 0, \
            'Empty hierarchy other.', 'Empty hierarchy other.', \
            'sha256:empty-other', 'chunker:v1');",
    )?;
    let empty_hierarchy = context_documents_json(
        &postgres,
        &ContextDocumentsQuery {
            document_id: "legi:empty-path-target@1804-02-21",
            as_of: None,
            include_siblings: true,
        },
    )?;
    let empty_hierarchy: serde_json::Value =
        serde_json::from_str(&empty_hierarchy).expect("context response is stable JSON");
    assert!(empty_hierarchy["siblings"].as_array().unwrap().is_empty());
    assert_eq!(empty_hierarchy["sibling_count"], 0);

    let before_validity = context_documents_json(
        &postgres,
        &ContextDocumentsQuery {
            document_id: "legi:LEGIARTI000006419320@1804-02-21",
            as_of: Some("1700-01-01"),
            include_siblings: true,
        },
    )?;
    let before_validity: serde_json::Value =
        serde_json::from_str(&before_validity).expect("context response is stable JSON");
    assert!(before_validity["target"].is_null());

    Ok(())
}
