mod common;

use common::discover_pg_config;
use jurisearch_ingest::legi::{CanonicalDocument, ParsedLegiXml, SourceProvenance, parse_legi_xml};
use jurisearch_storage::{
    projection::insert_legi_documents,
    retrieval::{ContextDocumentsQuery, context_documents_json},
    runtime::{ManagedPostgres, StorageError},
};

#[test]
fn legi_full_hierarchy_survives_parse_chunk_storage_and_context() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("LEGI structural survival")? else {
        return Ok(());
    };
    let expected_path = expected_path("Section 1 : Des formalites");
    let expected_context = format!(
        "{} > Article 1\n\nLe present article preserve une hierarchie complete pour le test.",
        expected_path.join(" > ")
    );
    // This test covers a CONTEXTE-native article hierarchy. Backfill-derived
    // hierarchy survival is covered by the LEGI metadata projection tests.
    let document = parse_article(
        "LEGIARTI000000000001",
        "1",
        "Section 1 : Des formalites",
        "Le present article preserve une hierarchie complete pour le test.",
    );
    let same_section_sibling = parse_article(
        "LEGIARTI000000000002",
        "2",
        "Section 1 : Des formalites",
        "Le present article partage la meme section profonde.",
    );
    let other_section_sibling = parse_article(
        "LEGIARTI000000000003",
        "3",
        "Section 2 : Des formalites voisines",
        "Le present article appartient a une section voisine.",
    );

    assert_eq!(document.hierarchy_path, expected_path);
    assert_eq!(document.chunks.len(), 1);
    assert_eq!(document.chunks[0].hierarchy_path, expected_path);
    assert_eq!(document.chunks[0].contextualized_body, expected_context);

    let root = tempfile::Builder::new()
        .prefix("jurisearch-structural-survival.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    let documents = vec![
        document.clone(),
        same_section_sibling,
        other_section_sibling,
    ];
    let insert_report = insert_legi_documents(&postgres, &documents, None)?;
    assert_eq!(insert_report.documents, 3);
    assert_eq!(insert_report.chunks, 3);

    let stored_chunk_path = postgres.execute_sql(
        "SELECT hierarchy_path::text \
         FROM chunks \
         WHERE document_id = 'legi:LEGIARTI000000000001@2024-01-01';",
    )?;
    assert!(stored_chunk_path.contains("Chapitre III : Des actes relatifs au mariage"));
    let stored_context = postgres.execute_sql(
        "SELECT contextualized_body \
         FROM chunks \
         WHERE document_id = 'legi:LEGIARTI000000000001@2024-01-01';",
    )?;
    assert_eq!(stored_context, expected_context);

    let context = context_documents_json(
        &postgres,
        &ContextDocumentsQuery {
            document_id: document.document_id.as_str(),
            as_of: Some("2024-01-01"),
            include_siblings: true,
        },
    )?;
    let context: serde_json::Value =
        serde_json::from_str(&context).expect("context response is stable JSON");
    let ancestry = context["ancestry"]
        .as_array()
        .expect("context ancestry is an array")
        .iter()
        .map(|entry| {
            entry["title"]
                .as_str()
                .expect("title is a string")
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(ancestry, expected_path);
    assert_eq!(context["target"]["title"], "Article 1");
    assert_eq!(
        context["target"]["hierarchy_path"][4],
        "Section 1 : Des formalites"
    );
    assert_eq!(context["sibling_count"], 1);
    assert_eq!(
        context["siblings"][0]["document_id"],
        "legi:LEGIARTI000000000002@2024-01-01"
    );

    Ok(())
}

fn expected_path(section_title: &str) -> Vec<String> {
    [
        "Code civil",
        "Livre Ier : Des personnes",
        "Titre II : Des actes de l'etat civil",
        "Chapitre III : Des actes relatifs au mariage",
        section_title,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn parse_article(id: &str, num: &str, section_title: &str, body: &str) -> CanonicalDocument {
    match parse_legi_xml(
        article_xml(id, num, section_title, body).as_str(),
        provenance(id),
    ) {
        Ok(ParsedLegiXml::Article(document)) => *document,
        Ok(other) => panic!("expected ARTICLE, got {}", other.root_name()),
        Err(error) => panic!("synthetic ARTICLE must parse: {error}"),
    }
}

fn provenance(id: &str) -> SourceProvenance {
    SourceProvenance {
        archive_name: Some("Freemium_legi_global.tar.gz".to_owned()),
        member_path: Some(format!("legi/articles/{id}.xml")),
        payload_hash: None,
    }
}

fn article_xml(id: &str, num: &str, section_title: &str, body: &str) -> String {
    format!(
        r#"
<ARTICLE>
  <META>
    <META_COMMUN>
      <ID>{id}</ID>
      <URL>/codes/article_lc/{id}</URL>
      <NATURE>Article</NATURE>
    </META_COMMUN>
    <META_ARTICLE>
      <NUM>{num}</NUM>
      <ETAT>VIGUEUR</ETAT>
      <TYPE>AUTONOME</TYPE>
      <DATE_DEBUT>2024-01-01</DATE_DEBUT>
      <DATE_FIN>2999-01-01</DATE_FIN>
    </META_ARTICLE>
  </META>
  <CONTEXTE>
    <TEXTE>
      <TITRE_TXT>Code civil</TITRE_TXT>
      <TM>
        <TITRE_TM>Livre Ier : Des personnes</TITRE_TM>
        <TM>
          <TITRE_TM>Titre II : Des actes de l'etat civil</TITRE_TM>
          <TM>
            <TITRE_TM>Chapitre III : Des actes relatifs au mariage</TITRE_TM>
            <TM>
              <TITRE_TM>{section_title}</TITRE_TM>
            </TM>
          </TM>
        </TM>
      </TM>
    </TEXTE>
  </CONTEXTE>
  <BLOC_TEXTUEL>
    <CONTENU>
      <p>{body}</p>
    </CONTENU>
  </BLOC_TEXTUEL>
</ARTICLE>
"#
    )
}
