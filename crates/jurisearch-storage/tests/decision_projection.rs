//! Managed-Postgres integration test for jurisprudence decision projection + retrieval.
//!
//! Skips when no local pgrx/pg_search-capable PostgreSQL is discoverable.

mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_ingest::archive::ArchiveSource;
use jurisearch_ingest::juri::{CanonicalDecision, ParsedJuriXml, parse_juri_xml};
use jurisearch_ingest::legi::SourceProvenance;
use jurisearch_storage::{
    projection::{ChunkEmbeddingInsert, insert_chunk_embeddings, insert_decision_documents},
    retrieval::{
        DecisionFilters, FetchDocumentsQuery, GroupBy, HybridCandidateQuery, RelatedQuery,
        RelatedRelation, RetrievalMode, RetrievalOptions, fetch_documents_json,
        hybrid_candidates_json, related_neighbours_json,
    },
    runtime::{ManagedPostgres, StorageError},
};

const EMBEDDING_FINGERPRINT: &str = "bge-m3:1024:normalize:true";

const JUDI_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<TEXTE_JURI_JUDI>
<META><META_COMMUN><ID>JURITEXT000051824029</ID><ANCIEN_ID/><ORIGINE>JURI</ORIGINE>
<URL>texte/juri/judi/JURI/TEXT/.../JURITEXT000051824029.xml</URL><NATURE>ARRET</NATURE>
</META_COMMUN><META_SPEC><META_JURI>
<TITRE>Cour de cassation, chambre sociale, 4 juin 2025, clause de non-concurrence</TITRE>
<DATE_DEC>2025-06-04</DATE_DEC><JURIDICTION>Cour de cassation</JURIDICTION>
<NUMERO>P2500111</NUMERO><SOLUTION>Cassation</SOLUTION>
</META_JURI><META_JURI_JUDI>
<NUMEROS_AFFAIRES><NUMERO_AFFAIRE>23-14999</NUMERO_AFFAIRE></NUMEROS_AFFAIRES>
<PUBLI_BULL publie="oui"/><FORMATION>CHAMBRE_SOCIALE</FORMATION>
<ECLI>ECLI:FR:CCASS:2025:SO00111</ECLI>
</META_JURI_JUDI></META_SPEC></META>
<TEXTE><BLOC_TEXTUEL><CONTENU>La clause de non-concurrence est nulle faute de contrepartie financière. En application de l'article L1234-5 du code du travail, la Cour casse l'arret attaque concernant M. [B].</CONTENU></BLOC_TEXTUEL>
<SOMMAIRE><SCT ID="1" TYPE="PRINCIPAL">CONTRAT DE TRAVAIL - clause de non-concurrence</SCT><ANA ID="1">La contrepartie financière est une condition de validité.</ANA></SOMMAIRE>
<CITATION_JP/></TEXTE>
<LIENS><LIEN id="LEGIARTI000006900782" cidtexte="LEGITEXT000006072050" sens="cible" typelien="CITATION" num="L1121-1" naturetexte="" nortexte="" numtexte="" datesignatexte="">Article L1121-1 du code du travail</LIEN></LIENS>
</TEXTE_JURI_JUDI>"#;

const ADMIN_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<TEXTE_JURI_ADMIN>
<META><META_COMMUN><ID>CETATEXT000051549953</ID><ANCIEN_ID/><ORIGINE>CETAT</ORIGINE>
<URL>texte/juri/admin/CETA/TEXT/.../CETATEXT000051549953.xml</URL><NATURE>Texte</NATURE>
</META_COMMUN><META_SPEC><META_JURI>
<TITRE>CAA de PARIS, 9ème chambre, 30/04/2025, titre de séjour</TITRE>
<DATE_DEC>2025-04-30</DATE_DEC><JURIDICTION>CAA de PARIS</JURIDICTION>
<NUMERO>24PA03561</NUMERO><SOLUTION/>
</META_JURI><META_JURI_ADMIN>
<FORMATION>9ème chambre</FORMATION><TYPE_REC>excès de pouvoir</TYPE_REC>
<PUBLI_RECUEIL>C</PUBLI_RECUEIL>
</META_JURI_ADMIN></META_SPEC></META>
<TEXTE><BLOC_TEXTUEL><CONTENU>Le refus de renouvellement du titre de séjour est légal.</CONTENU></BLOC_TEXTUEL><SOMMAIRE/></TEXTE>
<LIENS/>
</TEXTE_JURI_ADMIN>"#;

fn decision(source: ArchiveSource, xml: &str) -> CanonicalDecision {
    let provenance = SourceProvenance {
        archive_name: Some("test-archive.tar.gz".to_owned()),
        member_path: Some("member.xml".to_owned()),
        payload_hash: Some("sha256:testpayload".to_owned()),
    };
    match parse_juri_xml(source, xml, provenance).expect("parse decision") {
        ParsedJuriXml::Decision(decision) => *decision,
        other => panic!("expected decision, got {other:?}"),
    }
}

#[test]
fn decisions_project_search_and_fetch() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("decision projection")? else {
        return Ok(());
    };

    let judicial = decision(ArchiveSource::Cass, JUDI_XML);
    let administrative = decision(ArchiveSource::Jade, ADMIN_XML);
    let decisions = vec![judicial.clone(), administrative.clone()];

    let root = tempfile::Builder::new()
        .prefix("jurisearch-decision-projection.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    let report = insert_decision_documents(&postgres, &decisions, Some(EMBEDDING_FINGERPRINT))?;
    assert_eq!(report.documents, 2);
    assert!(report.chunks >= 3); // judicial: summary + body; administrative: body
    assert_eq!(report.publisher_edges, 1); // judicial LIEN; administrative has none

    // documents row: kind='decision', valid_from=decision_date, valid_to NULL.
    let doc_meta = postgres.execute_sql(
        "SELECT kind || '|' || coalesce(valid_from::text,'null') || '|' || coalesce(valid_to::text,'null') \
         FROM documents WHERE document_id = 'cass:JURITEXT000051824029';",
    )?;
    assert_eq!(doc_meta.trim(), "decision|2025-06-04|null");

    // publisher edge persisted with edge_source='publisher' and the resolved target uid in payload.
    let edge = postgres.execute_sql(
        "SELECT edge_source || '|' || coalesce(payload->>'to_source_uid','null') \
         FROM graph_edges \
         WHERE from_document_id = 'cass:JURITEXT000051824029' AND edge_source = 'publisher';",
    )?;
    assert_eq!(edge.trim(), "publisher|LEGIARTI000006900782");
    // The body reference to article L1234-5 produced a distinguishable inferred edge.
    let inferred = postgres.execute_sql(
        "SELECT count(*) FROM graph_edges \
         WHERE from_document_id = 'cass:JURITEXT000051824029' AND edge_source = 'inferred';",
    )?;
    assert_eq!(inferred.trim(), "1");

    // Embed every chunk: target (judicial) chunks get the target vector, the administrative decision
    // gets a decoy vector, so dense retrieval ranks the judicial decision first.
    let target_vector = vector_literal(0);
    let decoy_vector = vector_literal(1);
    let mut embeddings = Vec::new();
    for chunk in &judicial.chunks {
        embeddings.push(ChunkEmbeddingInsert {
            chunk_id: chunk.chunk_id.as_str(),
            embedding_fingerprint: EMBEDDING_FINGERPRINT,
            embedding_literal: target_vector.as_str(),
            model: "bge-m3",
            dimension: 1024,
        });
    }
    for chunk in &administrative.chunks {
        embeddings.push(ChunkEmbeddingInsert {
            chunk_id: chunk.chunk_id.as_str(),
            embedding_fingerprint: EMBEDDING_FINGERPRINT,
            embedding_literal: decoy_vector.as_str(),
            model: "bge-m3",
            dimension: 1024,
        });
    }
    assert_eq!(
        insert_chunk_embeddings(&postgres, &embeddings)?,
        embeddings.len()
    );

    // Hybrid search restricted to decisions, valid as of today, returns the judicial decision.
    let decision_query = HybridCandidateQuery {
        query_text: "clause de non-concurrence",
        query_embedding: Some(&target_vector),
        embedding_fingerprint: Some(EMBEDDING_FINGERPRINT),
        retrieval_mode: RetrievalMode::Hybrid,
        options: RetrievalOptions::default(),
        group_by: GroupBy::Document,
        after_cursor: None,
        as_of: "2025-12-31",
        kind_filter: Some("decision"),
        project_authority: false,
        decision_filters: DecisionFilters::default(),
        lexical_limit: 20,
        dense_limit: 20,
        limit: 5,
    };
    let response = hybrid_candidates_json(&postgres, &decision_query)?;
    let response: serde_json::Value = serde_json::from_str(&response)?;
    let top = &response["candidates"][0];
    assert_eq!(top["document_id"], "cass:JURITEXT000051824029");
    assert_eq!(top["kind"], "decision");
    assert_eq!(top["source"], "cass");
    assert_eq!(top["validity"]["from"], "2025-06-04");
    // A2 gate (OFF identity): project_authority=false must NOT emit a `publication` key, so the OFF
    // candidate payload is byte-identical to before the gate existed.
    assert!(
        top.get("publication").is_none(),
        "OFF projection must not expose publication; got {top:#}"
    );

    // A2 gate (ON projection): the SAME query with project_authority=true exposes the judicial
    // `PUBLI_BULL@publie` value ("oui" for this published Cassation decision) for the authority rerank.
    let projected = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            project_authority: true,
            ..decision_query
        },
    )?;
    let projected: serde_json::Value = serde_json::from_str(&projected)?;
    let projected_top = &projected["candidates"][0];
    assert_eq!(projected_top["document_id"], "cass:JURITEXT000051824029");
    assert_eq!(projected_top["publication"], "oui");

    // Temporal correctness: a decision is not "valid" before it was rendered.
    let before = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: "clause de non-concurrence",
            query_embedding: Some(&target_vector),
            embedding_fingerprint: Some(EMBEDDING_FINGERPRINT),
            retrieval_mode: RetrievalMode::Hybrid,
            options: RetrievalOptions::default(),
            group_by: GroupBy::Document,
            after_cursor: None,
            as_of: "2000-01-01",
            kind_filter: Some("decision"),
            project_authority: false,
            decision_filters: DecisionFilters::default(),
            lexical_limit: 20,
            dense_limit: 20,
            limit: 10,
        },
    )?;
    let before: serde_json::Value = serde_json::from_str(&before)?;
    assert!(
        before["candidates"]
            .as_array()
            .expect("candidates array")
            .is_empty(),
        "future decision leaked into as-of=2000-01-01: {before}"
    );

    // fetch returns the full decision text + chunk bodies.
    let fetch = fetch_documents_json(
        &postgres,
        &FetchDocumentsQuery {
            document_ids: &["cass:JURITEXT000051824029"],
        },
    )?;
    let fetch: serde_json::Value = serde_json::from_str(&fetch)?;
    let fetched = &fetch["documents"][0];
    assert_eq!(fetched["document_id"], "cass:JURITEXT000051824029");
    assert_eq!(fetched["body"], judicial.body);
    // Pseudonymisation preserved end-to-end through storage.
    assert!(fetched["body"].as_str().unwrap().contains("M. [B]"));

    postgres.stop()?;
    Ok(())
}

#[test]
fn decision_search_metadata_filters() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("decision search filters")? else {
        return Ok(());
    };

    let judicial = decision(ArchiveSource::Cass, JUDI_XML); // Cour de cassation, CHAMBRE_SOCIALE, oui, 2025-06-04
    let administrative = decision(ArchiveSource::Jade, ADMIN_XML); // CAA de PARIS, 9ème chambre, C, 2025-04-30
    let decisions = vec![judicial.clone(), administrative.clone()];

    let root = tempfile::Builder::new()
        .prefix("jurisearch-decision-filters.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    insert_decision_documents(&postgres, &decisions, Some(EMBEDDING_FINGERPRINT))?;

    // Give both decisions the SAME vector so ranking is filter-driven, not vector-driven.
    let vector = vector_literal(0);
    let embeddings: Vec<ChunkEmbeddingInsert> = judicial
        .chunks
        .iter()
        .chain(administrative.chunks.iter())
        .map(|chunk| ChunkEmbeddingInsert {
            chunk_id: chunk.chunk_id.as_str(),
            embedding_fingerprint: EMBEDDING_FINGERPRINT,
            embedding_literal: vector.as_str(),
            model: "bge-m3",
            dimension: 1024,
        })
        .collect();
    insert_chunk_embeddings(&postgres, &embeddings)?;

    let search = |filters: DecisionFilters| -> Vec<String> {
        let response = hybrid_candidates_json(
            &postgres,
            &HybridCandidateQuery {
                query_text: "decision",
                query_embedding: Some(&vector),
                embedding_fingerprint: Some(EMBEDDING_FINGERPRINT),
                retrieval_mode: RetrievalMode::Dense,
                options: RetrievalOptions::default(),
                group_by: GroupBy::Document,
                after_cursor: None,
                as_of: "2025-12-31",
                kind_filter: Some("decision"),
                project_authority: false,
                decision_filters: filters,
                lexical_limit: 20,
                dense_limit: 20,
                limit: 10,
            },
        )
        .expect("search");
        let response: serde_json::Value = serde_json::from_str(&response).unwrap();
        response["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .map(|candidate| candidate["document_id"].as_str().unwrap().to_owned())
            .collect()
    };

    // Court / jurisdiction substring.
    let cassation = search(DecisionFilters {
        jurisdiction: Some("cassation"),
        ..DecisionFilters::default()
    });
    assert_eq!(cassation, vec!["cass:JURITEXT000051824029".to_owned()]);

    let caa = search(DecisionFilters {
        jurisdiction: Some("CAA"),
        ..DecisionFilters::default()
    });
    assert_eq!(caa, vec!["jade:CETATEXT000051549953".to_owned()]);

    // Formation / chamber substring.
    let sociale = search(DecisionFilters {
        formation: Some("sociale"),
        ..DecisionFilters::default()
    });
    assert_eq!(sociale, vec!["cass:JURITEXT000051824029".to_owned()]);

    // Publication level (judicial PUBLI_BULL=oui).
    let published = search(DecisionFilters {
        publication: Some("oui"),
        ..DecisionFilters::default()
    });
    assert_eq!(published, vec!["cass:JURITEXT000051824029".to_owned()]);

    // Decision-date range excludes the earlier administrative decision (2025-04-30).
    let recent = search(DecisionFilters {
        decided_from: Some("2025-05-01"),
        ..DecisionFilters::default()
    });
    assert_eq!(recent, vec!["cass:JURITEXT000051824029".to_owned()]);

    // No filters returns both.
    assert_eq!(search(DecisionFilters::default()).len(), 2);

    // A LEGI article (version start 1990) must NOT be filtered by a decision-date bound: any decision
    // filter implies kind='decision', so the article is excluded entirely rather than date-filtered
    // by its version start.
    postgres.execute_sql(
        "INSERT INTO documents \
            (document_id, source, kind, source_uid, body, valid_from, source_payload_hash) \
         VALUES ('legi:LEGIARTI000000000001@1990-01-01', 'legi', 'article', \
                 'LEGIARTI000000000001', 'decision article body', '1990-01-01', 'sha256:a');",
    )?;
    postgres.execute_sql(
        "INSERT INTO chunks \
            (chunk_id, document_id, chunk_index, body, contextualized_body, chunk_kind, chunking, \
             boundary, source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('chunk:legi:LEGIARTI000000000001@1990-01-01:0', \
                 'legi:LEGIARTI000000000001@1990-01-01', 0, 'decision article body', \
                 'decision article body', 'article_body', 'structural', 'article', 'sha256:a', \
                 'x', 'bge-m3:1024:normalize:true');",
    )?;
    postgres.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('chunk:legi:LEGIARTI000000000001@1990-01-01:0', 'bge-m3:1024:normalize:true', \
                 '{}'::vector, 'bge-m3', 1024);",
        vector_literal(0)
    ))?;
    // kind=all + a decision-date bound that the 1990 article's version start satisfies: the article
    // must still be excluded (decision filters are decision-scoped), leaving only the 2025 decision.
    let response = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: "decision",
            query_embedding: Some(&vector),
            embedding_fingerprint: Some(EMBEDDING_FINGERPRINT),
            retrieval_mode: RetrievalMode::Dense,
            options: RetrievalOptions::default(),
            group_by: GroupBy::Document,
            after_cursor: None,
            as_of: "2025-12-31",
            kind_filter: None,
            project_authority: false,
            decision_filters: DecisionFilters {
                decided_from: Some("1980-01-01"),
                ..DecisionFilters::default()
            },
            lexical_limit: 20,
            dense_limit: 20,
            limit: 10,
        },
    )?;
    let response: serde_json::Value = serde_json::from_str(&response)?;
    let ids: Vec<&str> = response["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["document_id"].as_str().unwrap())
        .collect();
    assert!(
        !ids.contains(&"legi:LEGIARTI000000000001@1990-01-01"),
        "article leaked through a decision-date filter: {ids:?}"
    );
    assert!(
        ids.iter()
            .all(|id| id.starts_with("cass:") || id.starts_with("jade:"))
    );

    postgres.stop()?;
    Ok(())
}

#[test]
fn decision_graph_edges_and_interpreted_by() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("decision graph layer")? else {
        return Ok(());
    };

    let judicial = decision(ArchiveSource::Cass, JUDI_XML);
    let root = tempfile::Builder::new()
        .prefix("jurisearch-decision-graph.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // A LEGI article the decision officially cites (publisher LIEN id=LEGIARTI000006900782, cible).
    postgres.execute_sql(
        "INSERT INTO documents \
            (document_id, source, kind, source_uid, body, valid_from, source_payload_hash) \
         VALUES ('legi:LEGIARTI000006900782@1990-01-01', 'legi', 'article', \
                 'LEGIARTI000006900782', 'Article L1121-1 du code du travail.', \
                 '1990-01-01', 'sha256:article');",
    )?;

    insert_decision_documents(&postgres, &[judicial.clone()], None)?;

    // The decision projected BOTH a publisher edge (from LIENS) and an inferred edge (from the body
    // reference to article L1234-5), kept distinguishable by edge_source.
    let publisher = postgres.execute_sql(
        "SELECT count(*) FROM graph_edges \
         WHERE from_document_id = 'cass:JURITEXT000051824029' AND edge_source = 'publisher';",
    )?;
    assert_eq!(publisher.trim(), "1");
    let inferred = postgres.execute_sql(
        "SELECT count(*) FROM graph_edges \
         WHERE from_document_id = 'cass:JURITEXT000051824029' AND edge_source = 'inferred';",
    )?;
    assert_eq!(inferred.trim(), "1");
    let inferred_article = postgres.execute_sql(
        "SELECT payload->'attributes'->0->>'value' FROM graph_edges \
         WHERE from_document_id = 'cass:JURITEXT000051824029' AND edge_source = 'inferred';",
    )?;
    assert_eq!(inferred_article.trim(), "L1234-5");

    // interpreted_by: from the cited article, find the decision interpreting it.
    let interpreted = related_neighbours_json(
        &postgres,
        &RelatedQuery {
            document_id: "legi:LEGIARTI000006900782@1990-01-01",
            rel: RelatedRelation::InterpretedBy,
            limit: 10,
        },
    )?;
    let interpreted: serde_json::Value = serde_json::from_str(&interpreted)?;
    assert_eq!(interpreted["rel"], "interpreted_by");
    assert_eq!(interpreted["returned"], 1);
    let neighbour = &interpreted["neighbours"][0];
    assert_eq!(
        neighbour["document"]["document_id"],
        "cass:JURITEXT000051824029"
    );
    assert_eq!(neighbour["edge"]["edge_source"], "publisher");

    // cites: from the decision, find the article it applies (the inverse direction).
    let cites = related_neighbours_json(
        &postgres,
        &RelatedQuery {
            document_id: "cass:JURITEXT000051824029",
            rel: RelatedRelation::Cites,
            limit: 10,
        },
    )?;
    let cites: serde_json::Value = serde_json::from_str(&cites)?;
    assert_eq!(cites["returned"], 1);
    assert_eq!(
        cites["neighbours"][0]["document"]["document_id"],
        "legi:LEGIARTI000006900782@1990-01-01"
    );

    postgres.stop()?;
    Ok(())
}
