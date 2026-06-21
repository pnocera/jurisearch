mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_ingest::legi::{ParsedLegiXml, SourceProvenance, parse_legi_xml};
use jurisearch_storage::{
    projection::{
        LegiMetadataRoot, backfill_legi_article_hierarchy_from_metadata, insert_legi_metadata_roots,
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
      <TITRE_TXT debut="1804-03-21" fin="2999-01-01">Code civil</TITRE_TXT>
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
    <LIEN_TXT id="LEGITEXT000006070721" debut="1804-03-21"/>
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
            LegiMetadataRoot::TextStruct(text_struct.as_ref()),
        ],
    )?;
    assert_eq!(report.metadata_roots, 3);

    let report =
        insert_legi_metadata_roots(&postgres, &[LegiMetadataRoot::TextVersion(text.as_ref())])?;
    assert_eq!(report.metadata_roots, 1);

    assert_eq!(
        postgres.execute_sql("SELECT count(*)::text FROM legi_metadata_roots;")?,
        "3"
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
             legi:TEXTELR:LEGITEXT000006070721@1804-03-21:{text_struct_digest},\
             legi:TEXTE_VERSION:LEGITEXT000049371154@1956-04-12"
        )
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT parent_source_uid || ':' || (canonical_json->'hierarchy_path'->>0) \
             FROM legi_metadata_roots \
             WHERE root_kind = 'SECTION_TA';",
        )?,
        "LEGITEXT000006070721:Code civil"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT coalesce(canonical_json->>'nature', 'absent') \
             FROM legi_metadata_roots \
             WHERE root_kind = 'TEXTE_VERSION';",
        )?,
        "absent"
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
                              \"hierarchy_path\":[\"Code civil\"]}}]}}'); \
         INSERT INTO chunks \
            (chunk_id, document_id, chunk_index, body, source_payload_hash, \
             chunk_builder_version, embedding_fingerprint) \
         VALUES \
            ('chunk:legi:LEGIARTI000006419320@1804-02-21:0', \
             'legi:LEGIARTI000006419320@1804-02-21', 0, \
             'Tout fait quelconque de l''homme...', 'sha256:article-1240', \
             'chunker:v0', 'bge-m3:1024:normalize:true'); \
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
                \"to_source_uid\":\"LEGISCTA000006089696\"}}');",
        vector_literal(0)
    ))?;

    let backfill = backfill_legi_article_hierarchy_from_metadata(&postgres)?;
    assert_eq!(backfill.documents_updated, 1);
    assert_eq!(backfill.embeddings_invalidated, 1);
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
