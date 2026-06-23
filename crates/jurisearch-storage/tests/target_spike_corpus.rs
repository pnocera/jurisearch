mod common;

use std::time::{Duration, Instant};

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::{
    retrieval::{GroupBy, HybridCandidateQuery, RetrievalOptions, RetrievalMode, hybrid_candidates_json},
    runtime::{ManagedPostgres, StorageError},
};

const ARTICLE_FIXTURES: u32 = 50_000;
const DECISION_FIXTURES: u32 = 10_000;
const EMBEDDING_FINGERPRINT: &str = "bge-m3:1024:normalize:true";
const QUERY_TEXT: &str = "responsabilite faute dommage";
const TARGET_CHUNK_ID: &str = "chunk:legi:1240:0";
const WARM_LATENCY_BUDGET: Duration = Duration::from_millis(500);

#[test]
#[ignore = "target-scale storage spike; run explicitly after pg_search/pgvector setup"]
fn target_spike_corpus_retrieval_stays_under_latency_budget() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("target spike corpus retrieval")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-target-spike-pg.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let target_vector = vector_literal(0);
    let decoy_vector = vector_literal(1);

    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_target_spike_corpus(&postgres, &target_vector, &decoy_vector)?;

    let total_documents = postgres.execute_sql("SELECT count(*)::text FROM documents;")?;
    assert_eq!(
        total_documents,
        (ARTICLE_FIXTURES + DECISION_FIXTURES).to_string()
    );

    let request = HybridCandidateQuery {
        query_text: QUERY_TEXT,
        query_embedding: Some(&target_vector),
        embedding_fingerprint: Some(EMBEDDING_FINGERPRINT),
        retrieval_mode: RetrievalMode::Hybrid,
        options: RetrievalOptions::default(),
        group_by: GroupBy::Chunk,
        after_cursor: None,
        as_of: "2024-06-01",
        kind_filter: None,
        lexical_limit: 50,
        dense_limit: 50,
        limit: 8,
    };

    let lexical_started = Instant::now();
    let lexical_top = postgres.execute_sql(
        "SELECT c.chunk_id \
         FROM chunks c \
         JOIN documents d ON d.document_id = c.document_id \
         WHERE c.contextualized_body @@@ 'responsabilite faute dommage' \
           AND (d.valid_from IS NULL OR d.valid_from <= '2024-06-01'::date) \
           AND (d.valid_to IS NULL OR d.valid_to > '2024-06-01'::date) \
         ORDER BY paradedb.score(c.chunk_id) DESC, c.chunk_id \
         LIMIT 50;",
    )?;
    let lexical_elapsed = lexical_started.elapsed();

    let dense_started = Instant::now();
    let dense_top = postgres.execute_sql(&format!(
        "SET ivfflat.probes = 4; \
         SELECT chunk_id \
         FROM chunk_embeddings \
         WHERE embedding_fingerprint = '{}' \
         ORDER BY embedding <-> '{}'::vector \
         LIMIT 50;",
        EMBEDDING_FINGERPRINT, target_vector
    ))?;
    let dense_elapsed = dense_started.elapsed();
    eprintln!(
        "target_spike_component_ms lexical={:.2} dense={:.2}",
        lexical_elapsed.as_secs_f64() * 1000.0,
        dense_elapsed.as_secs_f64() * 1000.0
    );
    assert!(
        lexical_top
            .lines()
            .any(|chunk_id| chunk_id == TARGET_CHUNK_ID)
    );
    assert!(
        dense_top
            .lines()
            .any(|chunk_id| chunk_id == TARGET_CHUNK_ID)
    );

    let warmup = hybrid_candidates_json(&postgres, &request)?;
    assert_top_candidate(&warmup);

    let started = Instant::now();
    let response = hybrid_candidates_json(&postgres, &request)?;
    let elapsed = started.elapsed();
    assert_top_candidate(&response);
    eprintln!(
        "target_spike_documents={} warm_query_ms={:.2}",
        ARTICLE_FIXTURES + DECISION_FIXTURES,
        elapsed.as_secs_f64() * 1000.0
    );
    assert!(
        elapsed < WARM_LATENCY_BUDGET,
        "warm target-spike query took {elapsed:?}, budget is {WARM_LATENCY_BUDGET:?}"
    );

    Ok(())
}

fn seed_target_spike_corpus(
    postgres: &ManagedPostgres,
    target_vector: &str,
    decoy_vector: &str,
) -> Result<(), StorageError> {
    postgres.execute_sql(&format!(
        r#"
INSERT INTO documents
    (document_id, source, kind, source_uid, version_group, citation, title, body,
     valid_from, valid_to, source_payload_hash)
SELECT
    'legi:LEGIARTI' || lpad(n::text, 12, '0') || '@2024-01-01',
    'legi',
    'article',
    'LEGIARTI' || lpad(n::text, 12, '0'),
    'LEGIARTI' || lpad(n::text, 12, '0'),
    'Code civil article ' || n,
    'Article ' || n,
    CASE
        WHEN n = 1240 THEN 'responsabilite civile faute dommage reparation article 1240 responsabilite faute dommage'
        WHEN n BETWEEN 1 AND 10 THEN 'responsabilite civile faute dommage reparation version inactive ' || n
        WHEN n % 1000 = 0 THEN 'responsabilite administrative service public article ' || n
        ELSE 'article legislatif procedure obligations contrats fiscalite famille numero ' || n
    END,
    CASE WHEN n BETWEEN 6 AND 10 THEN '2025-01-01'::date ELSE '2024-01-01'::date END,
    CASE WHEN n BETWEEN 1 AND 5 THEN '2024-01-01'::date ELSE NULL::date END,
    'sha256:legi:' || n
FROM generate_series(1, {article_fixtures}) AS n;

INSERT INTO documents
    (document_id, source, kind, source_uid, citation, title, body,
     valid_from, source_payload_hash)
SELECT
    'judilibre:JURI' || lpad(n::text, 12, '0') || '@2024-01-01',
    'judilibre',
    'decision',
    'JURI' || lpad(n::text, 12, '0'),
    'Cass. civ. decision fixture ' || n,
    'Decision fixture ' || n,
    CASE
        WHEN n % 500 = 0 THEN 'decision responsabilite contractuelle procedure preuve numero ' || n
        ELSE 'decision contentieux commercial social penal procedure numero ' || n
    END,
    '2024-01-01',
    'sha256:judilibre:' || n
FROM generate_series(1, {decision_fixtures}) AS n;

INSERT INTO chunks
    (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash,
     chunk_builder_version, embedding_fingerprint)
SELECT
    'chunk:legi:' || n || ':0',
    'legi:LEGIARTI' || lpad(n::text, 12, '0') || '@2024-01-01',
    0,
    CASE
        WHEN n = 1240 THEN 'responsabilite civile faute dommage reparation article 1240 responsabilite faute dommage'
        WHEN n BETWEEN 1 AND 10 THEN 'responsabilite civile faute dommage reparation version inactive ' || n
        WHEN n % 1000 = 0 THEN 'responsabilite administrative service public article ' || n
        ELSE 'article legislatif procedure obligations contrats fiscalite famille numero ' || n
    END,
    -- Mirrors production's hierarchy/header prefix followed by raw chunk body.
    'Code civil > Article ' || n || E'\n' ||
    CASE
        WHEN n = 1240 THEN 'responsabilite civile faute dommage reparation article 1240 responsabilite faute dommage'
        WHEN n BETWEEN 1 AND 10 THEN 'responsabilite civile faute dommage reparation version inactive ' || n
        WHEN n % 1000 = 0 THEN 'responsabilite administrative service public article ' || n
        ELSE 'article legislatif procedure obligations contrats fiscalite famille numero ' || n
    END,
    'sha256:legi:' || n,
    'chunker:target-spike:v0',
    '{embedding_fingerprint}'
FROM generate_series(1, {article_fixtures}) AS n;

INSERT INTO chunks
    (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash,
     chunk_builder_version, embedding_fingerprint)
SELECT
    'chunk:judilibre:' || n || ':0',
    'judilibre:JURI' || lpad(n::text, 12, '0') || '@2024-01-01',
    0,
    CASE
        WHEN n % 500 = 0 THEN 'decision responsabilite contractuelle procedure preuve numero ' || n
        ELSE 'decision contentieux commercial social penal procedure numero ' || n
    END,
    -- Mirrors production's title/zone prefix followed by raw chunk body.
    'Decision fixture ' || n || E'\n' ||
    CASE
        WHEN n % 500 = 0 THEN 'decision responsabilite contractuelle procedure preuve numero ' || n
        ELSE 'decision contentieux commercial social penal procedure numero ' || n
    END,
    'sha256:judilibre:' || n,
    'chunker:target-spike:v0',
    '{embedding_fingerprint}'
FROM generate_series(1, {decision_fixtures}) AS n;

INSERT INTO chunk_embeddings
    (chunk_id, embedding_fingerprint, embedding, model, dimension)
SELECT
    c.chunk_id,
    '{embedding_fingerprint}',
    CASE
        WHEN c.chunk_id = '{target_chunk_id}'
          OR c.chunk_id IN (
              'chunk:legi:1:0',
              'chunk:legi:2:0',
              'chunk:legi:3:0',
              'chunk:legi:4:0',
              'chunk:legi:5:0',
              'chunk:legi:6:0',
              'chunk:legi:7:0',
              'chunk:legi:8:0',
              'chunk:legi:9:0',
              'chunk:legi:10:0'
          )
        THEN '{target_vector}'::vector
        ELSE '{decoy_vector}'::vector
    END,
    'bge-m3',
    1024
FROM chunks c;

CREATE INDEX chunk_embeddings_embedding_ivfflat_idx
ON chunk_embeddings USING ivfflat (embedding vector_l2_ops)
WITH (lists = 32);

ANALYZE documents;
ANALYZE chunks;
ANALYZE chunk_embeddings;
"#,
        article_fixtures = ARTICLE_FIXTURES,
        decision_fixtures = DECISION_FIXTURES,
        embedding_fingerprint = EMBEDDING_FINGERPRINT,
        target_chunk_id = TARGET_CHUNK_ID,
        target_vector = target_vector,
        decoy_vector = decoy_vector
    ))?;
    Ok(())
}

fn assert_top_candidate(response: &str) {
    let response: serde_json::Value =
        serde_json::from_str(response).expect("target spike response is stable JSON");
    assert_eq!(response["query"], QUERY_TEXT);
    assert_eq!(response["candidates"][0]["chunk_id"], TARGET_CHUNK_ID);
    assert_eq!(
        response["candidates"][0]["scores"]["lexical_rank"].as_u64(),
        Some(1)
    );
    assert_eq!(
        response["candidates"][0]["scores"]["dense_rank"].as_u64(),
        Some(1)
    );

    let candidates = response["candidates"]
        .as_array()
        .expect("candidates is an array");
    for inactive in 1..=10 {
        let inactive_chunk_id = format!("chunk:legi:{inactive}:0");
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate["chunk_id"] != inactive_chunk_id),
            "inactive temporal candidate {inactive_chunk_id} leaked into response"
        );
    }
}
