mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_ingest::legi::{ParsedLegiXml, SourceProvenance, parse_legi_xml};
use jurisearch_storage::{
    projection::{
        LegiHierarchyBackfillScope, LegiMetadataRoot,
        backfill_legi_article_hierarchy_from_metadata,
        backfill_legi_article_hierarchy_from_metadata_scoped, insert_legi_metadata_roots,
    },
    runtime::{ManagedPostgres, StorageError},
};

#[test]
fn persists_legi_metadata_roots_with_stable_keys() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("LEGI metadata projection")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-legi-metadata.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    let text = match parse_legi_xml(
        r#"
<TEXTE_VERSION>
  <META>
    <META_COMMUN>
      <ID>LEGITEXT000049371154</ID>
      <URL>/id/LEGITEXT000049371154</URL>
      <NATURE/>
    </META_COMMUN>
    <META_SPEC>
      <META_TEXTE_VERSION>
        <TITRE>Arrete du 12 avril 1956</TITRE>
        <ETAT>VIGUEUR</ETAT>
        <DATE_DEBUT>1956-04-12</DATE_DEBUT>
        <DATE_FIN>2999-01-01</DATE_FIN>
      </META_TEXTE_VERSION>
    </META_SPEC>
  </META>
</TEXTE_VERSION>
"#,
        provenance("legi/textes/LEGITEXT000049371154.xml"),
    )
    .unwrap()
    {
        ParsedLegiXml::TextVersion(text) => text,
        _ => panic!("expected TEXTE_VERSION"),
    };
    let section = match parse_legi_xml(
        r#"
<SECTION_TA>
  <ID>LEGISCTA000006089696</ID>
  <TITRE_TA>Titre preliminaire</TITRE_TA>
  <CONTEXTE>
    <TEXTE cid="LEGITEXT000006070721">
      <TITRE_TXT debut="1804-03-21" fin="2020-01-01">Code civil</TITRE_TXT>
    </TEXTE>
  </CONTEXTE>
</SECTION_TA>
"#,
        provenance("legi/sections/LEGISCTA000006089696.xml"),
    )
    .unwrap()
    {
        ParsedLegiXml::SectionTa(section) => section,
        _ => panic!("expected SECTION_TA"),
    };
    let contemporary_section = match parse_legi_xml(
        r#"
<SECTION_TA>
  <ID>LEGISCTA000006089696</ID>
  <TITRE_TA>Titre contemporain</TITRE_TA>
  <CONTEXTE>
    <TEXTE cid="LEGITEXT000006070721">
      <TITRE_TXT debut="2020-01-01" fin="2999-01-01">Code civil</TITRE_TXT>
    </TEXTE>
  </CONTEXTE>
</SECTION_TA>
"#,
        provenance("legi/sections/LEGISCTA000006089696-contemporary.xml"),
    )
    .unwrap()
    {
        ParsedLegiXml::SectionTa(section) => section,
        _ => panic!("expected SECTION_TA"),
    };
    let text_struct = match parse_legi_xml(
        r#"
<TEXTELR>
  <META>
    <META_COMMUN>
      <ID>LEGITEXT000006070721</ID>
      <URL>/id/LEGITEXT000006070721</URL>
      <NATURE>CODE</NATURE>
    </META_COMMUN>
    <META_SPEC>
      <META_TEXTE_CHRONICLE>
        <CID>LEGITEXT000006070721</CID>
        <NUM>civil</NUM>
        <DATE_PUBLI>1804-03-21</DATE_PUBLI>
        <DATE_TEXTE>1804-03-21</DATE_TEXTE>
      </META_TEXTE_CHRONICLE>
    </META_SPEC>
  </META>
  <STRUCT>
    <LIEN_TXT cid="LEGITEXT999999999999" debut="1804-03-21" id="LEGITEXT000006070721"/>
    <LIEN_ART debut="1804-03-21" id="LEGIARTI000052000004"/>
    <LIEN_SECTION_TA cid="LEGISCTA000006000001" debut="1804-03-21" id="LEGISCTA000006000001" niv="1">Livre III</LIEN_SECTION_TA>
    <LIEN_SECTION_TA cid="LEGISCTA000006089696" debut="1804-03-21" id="LEGISCTA000006089696" niv="2">Titre preliminaire</LIEN_SECTION_TA>
    <LIEN_ART debut="2020-01-01" id="LEGIARTI000006419320"/>
    <LIEN_ART debut="1804-03-21" id="LEGIARTI000052000003"/>
  </STRUCT>
</TEXTELR>
"#,
        provenance("legi/textelr/LEGITEXT000006070721.xml"),
    )
    .unwrap()
    {
        ParsedLegiXml::TextStruct(text_struct) => text_struct,
        _ => panic!("expected TEXTELR"),
    };

    let report = insert_legi_metadata_roots(
        &postgres,
        &[
            LegiMetadataRoot::TextVersion(text.as_ref()),
            LegiMetadataRoot::SectionTa(section.as_ref()),
            LegiMetadataRoot::SectionTa(contemporary_section.as_ref()),
            LegiMetadataRoot::TextStruct(text_struct.as_ref()),
        ],
    )?;
    assert_eq!(report.metadata_roots, 4);

    let report =
        insert_legi_metadata_roots(&postgres, &[LegiMetadataRoot::TextVersion(text.as_ref())])?;
    assert_eq!(report.metadata_roots, 1);

    assert_eq!(
        postgres.execute_sql("SELECT count(*)::text FROM legi_metadata_roots;")?,
        "4"
    );
    let text_struct_digest = text_struct
        .source_payload_hash
        .strip_prefix("sha256:")
        .unwrap_or(text_struct.source_payload_hash.as_str());
    assert_eq!(
        postgres.execute_sql(
            "SELECT string_agg(metadata_key, ',' ORDER BY metadata_key) \
             FROM legi_metadata_roots;",
        )?,
        format!(
            "legi:SECTION_TA:LEGISCTA000006089696@1804-03-21,\
             legi:SECTION_TA:LEGISCTA000006089696@2020-01-01,\
             legi:TEXTELR:LEGITEXT000006070721@1804-03-21:{text_struct_digest},\
             legi:TEXTE_VERSION:LEGITEXT000049371154@1956-04-12"
        )
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT parent_source_uid || ':' || (canonical_json->'hierarchy_path'->>0) \
             FROM legi_metadata_roots \
             WHERE metadata_key = 'legi:SECTION_TA:LEGISCTA000006089696@1804-03-21';",
        )?,
        "LEGITEXT000006070721:Code civil"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT valid_to::text \
             FROM legi_metadata_roots \
             WHERE metadata_key = 'legi:SECTION_TA:LEGISCTA000006089696@1804-03-21';",
        )?,
        "2020-01-01"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->>'title' \
             FROM legi_metadata_roots \
             WHERE metadata_key = 'legi:SECTION_TA:LEGISCTA000006089696@2020-01-01';",
        )?,
        "Titre contemporain"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT coalesce(canonical_json->>'nature', 'absent') \
             FROM legi_metadata_roots \
             WHERE root_kind = 'TEXTE_VERSION';",
        )?,
        "absent"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->'structure_links'->0->>'source_tag' || ':' || \
                    coalesce(canonical_json->'structure_links'->0->>'target_source_uid', 'absent') || ':' || \
                    coalesce(canonical_json->'structure_links'->0->>'debut', 'absent') \
             FROM legi_metadata_roots \
             WHERE root_kind = 'TEXTELR';",
        )?,
        "LIEN_TXT:LEGITEXT000006070721:1804-03-21"
    );

    postgres.execute_sql(&format!(
        "INSERT INTO documents \
            (document_id, source, kind, source_uid, version_group, citation, title, body, \
             valid_from, source_payload_hash, canonical_json) \
         VALUES \
            ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
             'LEGIARTI000006419320', 'LEGIARTI000006419320', 'Code civil article 1240', \
             'Article 1240', 'Tout fait quelconque de l''homme...', '1804-02-21', \
             'sha256:article-1240', \
             '{{\"title\":\"Article 1240\",\"body\":\"Tout fait quelconque de l''homme...\",\
                \"hierarchy_path\":[\"Code civil\"],\
                \"chunks\":[{{\"body\":\"Tout fait quelconque de l''homme...\",\
                              \"contextualized_body\":\"Code civil > Article 1240\\n\\nTout fait quelconque de l''homme...\",\
                              \"hierarchy_path\":[\"Code civil\"]}}]}}'), \
            ('legi:LEGIARTI000052000001@2020-01-01', 'legi', 'article', \
             'LEGIARTI000052000001', 'LEGIARTI000052000001', 'Code civil article 1240 bis', \
             'Article 1240 bis', 'Version contemporaine...', '2020-01-01', \
             'sha256:article-boundary', \
             '{{\"title\":\"Article 1240 bis\",\"body\":\"Version contemporaine...\",\
                \"hierarchy_path\":[\"Code civil\"],\
                \"chunks\":[{{\"body\":\"Version contemporaine...\",\
                              \"contextualized_body\":\"Code civil > Article 1240 bis\\n\\nVersion contemporaine...\",\
                              \"hierarchy_path\":[\"Code civil\"]}}]}}'), \
            ('legi:LEGIARTI000052000002@1804-03-21', 'legi', 'article', \
             'LEGIARTI000052000002', 'LEGIARTI000052000002', 'Code civil article 1241', \
             'Article 1241', 'Article hors perimetre...', '1804-03-21', \
             'sha256:article-out-of-scope', \
             '{{\"title\":\"Article 1241\",\"body\":\"Article hors perimetre...\",\
                \"hierarchy_path\":[\"Code civil\"],\
                \"chunks\":[{{\"body\":\"Article hors perimetre...\",\
                              \"contextualized_body\":\"Code civil > Article 1241\\n\\nArticle hors perimetre...\",\
                              \"hierarchy_path\":[\"Code civil\"]}}]}}'), \
            ('legi:LEGIARTI000052000003@1804-03-21', 'legi', 'article', \
             'LEGIARTI000052000003', 'LEGIARTI000052000003', 'Code civil article 1242', \
             'Article 1242', 'Article seulement relie par TEXTELR...', '1804-03-21', \
             'sha256:article-textelr-linked', \
             '{{\"title\":\"Article 1242\",\"body\":\"Article seulement relie par TEXTELR...\",\
                \"hierarchy_path\":[\"Code civil\"],\
                \"chunks\":[{{\"body\":\"Article seulement relie par TEXTELR...\",\
                              \"contextualized_body\":\"Code civil > Article 1242\\n\\nArticle seulement relie par TEXTELR...\",\
                              \"hierarchy_path\":[\"Code civil\"]}}]}}'), \
            ('legi:LEGIARTI000052000004@1804-03-21', 'legi', 'article', \
             'LEGIARTI000052000004', 'LEGIARTI000052000004', 'Code civil article 1243', \
             'Article 1243', 'Article avant section TEXTELR...', '1804-03-21', \
             'sha256:article-textelr-no-section', \
             '{{\"title\":\"Article 1243\",\"body\":\"Article avant section TEXTELR...\",\
                \"hierarchy_path\":[\"Code civil\"],\
                \"chunks\":[{{\"body\":\"Article avant section TEXTELR...\",\
                              \"contextualized_body\":\"Code civil > Article 1243\\n\\nArticle avant section TEXTELR...\",\
                              \"hierarchy_path\":[\"Code civil\"]}}]}}'); \
         INSERT INTO chunks \
            (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
             chunk_builder_version, embedding_fingerprint) \
         VALUES \
            ('chunk:legi:LEGIARTI000006419320@1804-02-21:0', \
             'legi:LEGIARTI000006419320@1804-02-21', 0, \
             'Tout fait quelconque de l''homme...', \
             'Code civil > Article 1240\n\nTout fait quelconque de l''homme...', \
             'sha256:article-1240', \
             'chunker:v0', 'bge-m3:1024:normalize:true'), \
            ('chunk:legi:LEGIARTI000052000001@2020-01-01:0', \
             'legi:LEGIARTI000052000001@2020-01-01', 0, \
             'Version contemporaine...', \
             'Code civil > Article 1240 bis\n\nVersion contemporaine...', \
             'sha256:article-boundary', \
             'chunker:v0', NULL), \
            ('chunk:legi:LEGIARTI000052000002@1804-03-21:0', \
             'legi:LEGIARTI000052000002@1804-03-21', 0, \
             'Article hors perimetre...', \
             'Code civil > Article 1241\n\nArticle hors perimetre...', \
             'sha256:article-out-of-scope', \
             'chunker:v0', NULL), \
            ('chunk:legi:LEGIARTI000052000003@1804-03-21:0', \
             'legi:LEGIARTI000052000003@1804-03-21', 0, \
             'Article seulement relie par TEXTELR...', \
             'Code civil > Article 1242\n\nArticle seulement relie par TEXTELR...', \
             'sha256:article-textelr-linked', \
             'chunker:v0', NULL), \
            ('chunk:legi:LEGIARTI000052000004@1804-03-21:0', \
             'legi:LEGIARTI000052000004@1804-03-21', 0, \
             'Article avant section TEXTELR...', \
             'Code civil > Article 1243\n\nArticle avant section TEXTELR...', \
             'sha256:article-textelr-no-section', \
             'chunker:v0', NULL); \
         INSERT INTO chunk_embeddings \
            (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES \
            ('chunk:legi:LEGIARTI000006419320@1804-02-21:0', \
             'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024); \
         INSERT INTO graph_edges \
            (edge_id, from_document_id, edge_kind, edge_source, payload) \
         VALUES \
            ('edge:article-section', 'legi:LEGIARTI000006419320@1804-02-21', \
             'refers_to', 'publisher', \
             '{{\"source_tag\":\"LIEN_SECTION_TA\",\
                \"to_source_uid\":\"LEGISCTA000006089696\",\
                \"attributes\":[{{\"key\":\"debut\",\"value\":\"1804-03-21\"}}]}}'), \
            ('edge:article-section-contemporary', \
             'legi:LEGIARTI000052000001@2020-01-01', \
             'refers_to', 'publisher', \
             '{{\"source_tag\":\"LIEN_SECTION_TA\",\
                \"to_source_uid\":\"LEGISCTA000006089696\",\
                \"attributes\":[{{\"key\":\"debut\",\"value\":\"2020-01-01\"}}]}}'), \
            ('edge:article-section-out-of-scope', \
             'legi:LEGIARTI000052000002@1804-03-21', \
             'refers_to', 'publisher', \
             '{{\"source_tag\":\"LIEN_SECTION_TA\",\
                \"to_source_uid\":\"LEGISCTA000006089696\",\
                \"attributes\":[{{\"key\":\"debut\",\"value\":\"1804-03-21\"}}]}}');",
        vector_literal(0)
    ))?;

    let backfill = backfill_legi_article_hierarchy_from_metadata_scoped(
        &postgres,
        &LegiHierarchyBackfillScope {
            document_ids: vec![
                "legi:LEGIARTI000006419320@1804-02-21".to_owned(),
                "legi:LEGIARTI000052000001@2020-01-01".to_owned(),
            ],
            section_source_uids: Vec::new(),
            text_source_uids: Vec::new(),
        },
        None,
    )?;
    assert_eq!(backfill.documents_updated, 2);
    assert_eq!(backfill.embeddings_invalidated, 1);
    let repeated_backfill = backfill_legi_article_hierarchy_from_metadata_scoped(
        &postgres,
        &LegiHierarchyBackfillScope {
            document_ids: vec![
                "legi:LEGIARTI000006419320@1804-02-21".to_owned(),
                "legi:LEGIARTI000052000001@2020-01-01".to_owned(),
            ],
            section_source_uids: Vec::new(),
            text_source_uids: Vec::new(),
        },
        None,
    )?;
    assert_eq!(repeated_backfill.documents_updated, 0);
    assert_eq!(repeated_backfill.embeddings_invalidated, 0);
    // Direct publisher section edges win over TEXTELR candidates for the same article.
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->'hierarchy_path'->>1 \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?,
        "Titre preliminaire"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->'hierarchy_path'->>1 \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000052000001@2020-01-01';",
        )?,
        "Titre contemporain"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT coalesce(canonical_json->'hierarchy_path'->>1, 'absent') \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000052000002@1804-03-21';",
        )?,
        "absent"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT coalesce(canonical_json->'hierarchy_path'->>1, 'absent') \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000052000003@1804-03-21';",
        )?,
        "absent"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT coalesce(canonical_json->'hierarchy_path'->>1, 'absent') \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000052000004@1804-03-21';",
        )?,
        "absent"
    );
    let text_struct_backfill = backfill_legi_article_hierarchy_from_metadata_scoped(
        &postgres,
        &LegiHierarchyBackfillScope {
            document_ids: Vec::new(),
            section_source_uids: Vec::new(),
            text_source_uids: vec!["LEGITEXT000006070721".to_owned()],
        },
        None,
    )?;
    assert_eq!(text_struct_backfill.documents_updated, 1);
    assert_eq!(text_struct_backfill.embeddings_invalidated, 0);
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->'hierarchy_path'->>1 \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000052000003@1804-03-21';",
        )?,
        "Livre III"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->'hierarchy_path'->>2 \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000052000003@1804-03-21';",
        )?,
        "Titre preliminaire"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->'hierarchy_path'->>1 || ':' || \
                    coalesce(canonical_json->'hierarchy_path'->>2, 'absent') \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?,
        "Titre preliminaire:absent"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT coalesce(canonical_json->'hierarchy_path'->>1, 'absent') \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000052000004@1804-03-21';",
        )?,
        "absent"
    );
    let repeated_text_struct_backfill = backfill_legi_article_hierarchy_from_metadata_scoped(
        &postgres,
        &LegiHierarchyBackfillScope {
            document_ids: Vec::new(),
            section_source_uids: Vec::new(),
            text_source_uids: vec!["LEGITEXT000006070721".to_owned()],
        },
        None,
    )?;
    assert_eq!(repeated_text_struct_backfill.documents_updated, 0);
    assert_eq!(repeated_text_struct_backfill.embeddings_invalidated, 0);
    let full_backfill = backfill_legi_article_hierarchy_from_metadata(&postgres, None)?;
    assert_eq!(full_backfill.documents_updated, 1);
    assert_eq!(full_backfill.embeddings_invalidated, 0);
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->'hierarchy_path'->>1 \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000052000002@1804-03-21';",
        )?,
        "Titre preliminaire"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text FROM chunk_embeddings \
             WHERE chunk_id = 'chunk:legi:LEGIARTI000006419320@1804-02-21:0';",
        )?,
        "0"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT coalesce(embedding_fingerprint, 'null') \
             FROM chunks \
             WHERE chunk_id = 'chunk:legi:LEGIARTI000006419320@1804-02-21:0';",
        )?,
        "null"
    );

    Ok(())
}

fn provenance(member_path: &str) -> SourceProvenance {
    SourceProvenance {
        archive_name: Some("Freemium_legi_global_20250101-000000.tar.gz".to_owned()),
        member_path: Some(member_path.to_owned()),
        payload_hash: None,
    }
}
