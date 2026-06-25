//! Unit tests for the LEGI parser/canonicalizer. Moved out of mod.rs verbatim
//! (raw-string XML fixtures preserved exactly); `use super::*` resolves the legi items.

use std::path::PathBuf;

use crate::archive::ArchiveMember;

use super::{
    CanonicalDocument, CanonicalValidationError, LEGI_ARTICLE_CONTEXTUALIZED_CHUNK_MAX_CHARS,
    LegiParseError, ParsedLegiXml, SourceProvenance, extract_known_source_uid, parse_legi_member,
    parse_legi_xml, source_payload_hash,
};

#[test]
fn parses_official_article_to_canonical_document() {
    let document = parse_article_fixture(&article_fixture()).unwrap();

    assert_eq!(document.document_id, "legi:LEGIARTI000006419320@1804-02-21");
    assert_eq!(document.kind, "article");
    assert_eq!(document.source_uid, "LEGIARTI000006419320");
    assert_eq!(document.source_status.as_deref(), Some("VIGUEUR"));
    assert_eq!(document.source_nature.as_deref(), Some("Article"));
    assert_eq!(document.source_article_type.as_deref(), Some("AUTONOME"));
    assert_eq!(
        document.version_group.as_deref(),
        Some("LEGIARTI000006419320")
    );
    assert!(document.validate().is_ok());
    assert_eq!(document.valid_to, None);
    assert_eq!(document.valid_to_raw.as_deref(), Some("2999-01-01"));
    assert_eq!(document.title.as_deref(), Some("Article 1240"));
    assert!(document.body.contains("Tout fait quelconque de l'homme"));
    assert_eq!(
        document.hierarchy_path,
        vec![
            "Code civil".to_owned(),
            "Livre III : Des differentes manieres dont on acquiert la propriete".to_owned(),
            "Titre IV : Des engagements qui se forment sans convention".to_owned(),
        ]
    );
    assert_eq!(document.chunks.len(), 1);
    let chunk = &document.chunks[0];
    assert_eq!(
        chunk.chunk_id,
        "chunk:legi:LEGIARTI000006419320@1804-02-21:0"
    );
    assert_eq!(chunk.document_id, document.document_id);
    assert_eq!(chunk.chunk_index, 0);
    assert_eq!(chunk.body, document.body);
    assert!(chunk.contextualized_body.starts_with("Code civil >"));
    assert!(chunk.contextualized_body.contains("Article 1240\n\n"));
    assert_eq!(chunk.chunk_kind, "article_body");
    assert_eq!(chunk.chunking, "structural");
    assert_eq!(chunk.boundary, "article");
    assert_eq!(chunk.source_fields, vec!["BLOC_TEXTUEL/CONTENU"]);
    assert_eq!(chunk.source_payload_hash, document.source_payload_hash);
    assert_eq!(chunk.chunk_builder_version, "legi_article_structural:v2");
    assert_eq!(chunk.hierarchy_path, document.hierarchy_path);
    assert_eq!(document.publisher_edges.len(), 1);
    let edge = &document.publisher_edges[0];
    assert_eq!(edge.from_document_id, document.document_id);
    assert_eq!(edge.from_source_uid, document.source_uid);
    assert_eq!(edge.to_source_uid.as_deref(), Some("LEGIARTI000006554637"));
    assert_eq!(edge.to_document_id, None);
    assert_eq!(edge.relation, "refers_to");
    assert_eq!(edge.edge_source, "publisher");
    assert_eq!(edge.source_tag, "LIEN");
    assert_eq!(
        edge.source_text.as_deref(),
        Some("Decret no 73-138 - art. 11")
    );
    assert!(!document.body.contains("Decret no 73-138"));
    assert_eq!(edge.source_payload_hash, document.source_payload_hash);
    assert_eq!(edge.source_archive, document.source_archive);
    assert_eq!(edge.source_member_path, document.source_member_path);
    assert!(
        edge.attributes
            .iter()
            .any(|attribute| attribute.key == "typelien" && attribute.value == "MODIFICATION")
    );
    assert!(edge.edge_id.starts_with("publisher-edge:"));
    assert_eq!(
        document.source_archive.as_deref(),
        Some("Freemium_legi_global.tar.gz")
    );
    assert_eq!(
        document.source_member_path.as_deref(),
        Some("legi/articles/LEGIARTI.xml")
    );
    assert!(document.source_payload_hash.starts_with("sha256:"));
}

#[test]
fn preserves_article_status_and_temporal_variants() {
    let cases = [
        ("VIGUEUR", "2999-01-01", None),
        ("VIGUEUR", "2999-12-31", None),
        ("MODIFIE", "2016-10-01", Some("2016-10-01")),
        ("ABROGE", "2010-05-15", Some("2010-05-15")),
        ("ABROGE_DIFF", "2027-01-01", Some("2027-01-01")),
        ("TRANSFERE", "2012-01-01", Some("2012-01-01")),
    ];

    for (status, date_fin, expected_valid_to) in cases {
        let xml = article_fixture()
            .replace("<ETAT>VIGUEUR</ETAT>", &format!("<ETAT>{status}</ETAT>"))
            .replace(
                "<DATE_FIN>2999-01-01</DATE_FIN>",
                &format!("<DATE_FIN>{date_fin}</DATE_FIN>"),
            );
        let document = parse_article_fixture(&xml).unwrap();

        assert_eq!(document.source_status.as_deref(), Some(status));
        assert_eq!(document.document_id, "legi:LEGIARTI000006419320@1804-02-21");
        assert_eq!(document.valid_from, "1804-02-21");
        assert_eq!(document.valid_to.as_deref(), expected_valid_to);
        assert_eq!(document.valid_to_raw.as_deref(), Some(date_fin));
        assert_eq!(
            document.canonical_version,
            format!("legi_article:v2:nature=Article:etat={status}:type=AUTONOME")
        );
    }
}

#[test]
fn preserves_entities_and_inline_text_continuity() {
    let xml = article_fixture().replace(
        "<p>Tout fait quelconque de l'homme, qui cause a autrui un dommage, oblige celui par la faute duquel il est arrive a le reparer.</p>",
        "<p>Droit &amp; obligations &lt;ref&gt; caf&#233; <i>inline</i> suite</p>",
    );
    let document = parse_article_fixture(&xml).unwrap();

    assert_eq!(document.body, "Droit & obligations <ref> café inline suite");
    assert!(!document.body.contains("Droit  obligations"));
    assert!(!document.body.contains("inline\nsuite"));
}

#[test]
fn preserves_body_block_boundaries_for_structural_chunks() {
    let xml = article_fixture().replace(
        "<p>Tout fait quelconque de l'homme, qui cause a autrui un dommage, oblige celui par la faute duquel il est arrive a le reparer.</p>",
        "<p>Premier alinea.</p><p>Second alinea avec <i>inline</i>.</p><br/><p>Troisieme alinea.</p>",
    );
    let document = parse_article_fixture(&xml).unwrap();

    assert_eq!(
        document.body,
        "Premier alinea.\nSecond alinea avec inline.\nTroisieme alinea."
    );
    assert_eq!(document.chunks.len(), 1);
    assert_eq!(document.chunks[0].body, document.body);
    assert_eq!(document.chunks[0].chunking, "structural");
    assert_eq!(document.chunks[0].boundary, "article");
}

#[test]
fn long_articles_split_only_on_structural_alinea_boundaries() {
    let first = format!("Premier alinea. {}", vec!["alpha"; 550].join(" "));
    let second = format!("Deuxieme alinea. {}", vec!["beta"; 550].join(" "));
    let third = format!("Troisieme alinea. {}", vec!["gamma"; 550].join(" "));
    let replacement = format!("<p>{first}</p><p>{second}</p><p>{third}</p>");
    let xml = article_fixture().replace(
        "<p>Tout fait quelconque de l'homme, qui cause a autrui un dommage, oblige celui par la faute duquel il est arrive a le reparer.</p>",
        &replacement,
    );
    let document = parse_article_fixture(&xml).unwrap();

    assert_eq!(
        document.body,
        format!("{first}\n{second}\n{third}").as_str()
    );
    assert_eq!(document.chunks.len(), 3);

    for (index, expected_body) in [first, second, third].iter().enumerate() {
        let chunk = &document.chunks[index];
        assert_eq!(
            chunk.chunk_id,
            format!("chunk:{}:{index}", document.document_id)
        );
        assert_eq!(chunk.chunk_index, index);
        assert_eq!(chunk.body, *expected_body);
        assert_eq!(chunk.chunking, "structural");
        assert_eq!(chunk.boundary, "alinea");
        assert_eq!(
            chunk.source_fields,
            vec![
                "BLOC_TEXTUEL/CONTENU".to_owned(),
                format!("BLOC_TEXTUEL/CONTENU/alinea:{}-{}", index + 1, index + 1),
            ]
        );
        assert!(chunk.contextualized_body.starts_with("Code civil >"));
        assert!(chunk.contextualized_body.ends_with(expected_body));
        assert_eq!(chunk.chunk_builder_version, "legi_article_structural:v2");
    }
}

#[test]
fn single_oversized_alinea_is_not_hard_split() {
    let body = format!("Unique alinea. {}", vec!["alpha"; 1_200].join(" "));
    let replacement = format!("<p>{body}</p>");
    let xml = article_fixture().replace(
        "<p>Tout fait quelconque de l'homme, qui cause a autrui un dommage, oblige celui par la faute duquel il est arrive a le reparer.</p>",
        &replacement,
    );
    let document = parse_article_fixture(&xml).unwrap();

    assert_eq!(document.body, body);
    assert_eq!(document.chunks.len(), 1);
    let chunk = &document.chunks[0];
    assert!(
        chunk.contextualized_body.chars().count() > LEGI_ARTICLE_CONTEXTUALIZED_CHUNK_MAX_CHARS
    );
    assert_eq!(chunk.body, document.body);
    assert_eq!(chunk.boundary, "article");
    assert_eq!(chunk.source_fields, vec!["BLOC_TEXTUEL/CONTENU"]);
    assert_eq!(chunk.chunk_builder_version, "legi_article_structural:v2");
}

#[test]
fn long_articles_pack_multiple_alineas_into_range_chunks() {
    let first = format!("Premier alinea. {}", vec!["alpha"; 400].join(" "));
    let second = format!("Deuxieme alinea. {}", vec!["beta"; 400].join(" "));
    let third = format!("Troisieme alinea. {}", vec!["gamma"; 400].join(" "));
    let fourth = format!("Quatrieme alinea. {}", vec!["delta"; 400].join(" "));
    let replacement = format!("<p>{first}</p><p>{second}</p><p>{third}</p><p>{fourth}</p>");
    let xml = article_fixture().replace(
        "<p>Tout fait quelconque de l'homme, qui cause a autrui un dommage, oblige celui par la faute duquel il est arrive a le reparer.</p>",
        &replacement,
    );
    let document = parse_article_fixture(&xml).unwrap();

    assert_eq!(
        document.body,
        format!("{first}\n{second}\n{third}\n{fourth}").as_str()
    );
    assert_eq!(document.chunks.len(), 2);

    let expected = [
        (
            format!("{first}\n{second}"),
            "BLOC_TEXTUEL/CONTENU/alinea:1-2",
        ),
        (
            format!("{third}\n{fourth}"),
            "BLOC_TEXTUEL/CONTENU/alinea:3-4",
        ),
    ];
    for (index, (expected_body, expected_source_field)) in expected.iter().enumerate() {
        let chunk = &document.chunks[index];
        assert_eq!(
            chunk.chunk_id,
            format!("chunk:{}:{index}", document.document_id)
        );
        assert_eq!(chunk.chunk_index, index);
        assert_eq!(chunk.body, *expected_body);
        assert_eq!(chunk.boundary, "alinea_range");
        assert_eq!(
            chunk.source_fields,
            vec![
                "BLOC_TEXTUEL/CONTENU".to_owned(),
                (*expected_source_field).to_owned(),
            ]
        );
        assert!(chunk.contextualized_body.ends_with(expected_body));
        assert_eq!(chunk.chunk_builder_version, "legi_article_structural:v2");
    }
}

#[test]
fn validation_rejects_broken_chunk_contract() {
    let mut document = parse_article_fixture(&article_fixture()).unwrap();
    document.chunks[0].chunk_id = "chunk:wrong".to_owned();

    assert!(matches!(
        document.validate(),
        Err(CanonicalValidationError::InvalidChunk { .. })
    ));
}

#[test]
fn extracts_inline_anchor_publisher_edges() {
    let xml = article_fixture().replace(
        "<p>Tout fait quelconque de l'homme, qui cause a autrui un dommage, oblige celui par la faute duquel il est arrive a le reparer.</p>",
        r#"<p>Voir <a href="/codes/article_lc/LEGIARTI000006419321">article suivant</a>.</p>"#,
    );
    let document = parse_article_fixture(&xml).unwrap();

    let anchor_edge = document
        .publisher_edges
        .iter()
        .find(|edge| edge.source_tag == "a")
        .expect("expected inline anchor edge");

    assert_eq!(
        anchor_edge.to_source_uid.as_deref(),
        Some("LEGIARTI000006419321")
    );
    assert_eq!(anchor_edge.source_text.as_deref(), Some("article suivant"));
    assert_eq!(anchor_edge.edge_source, "publisher");
    assert!(document.body.contains("Voir article suivant."));
}

#[test]
fn extracts_all_dila_publisher_link_tags() {
    let xml = article_fixture().replace(
        "  </LIENS>",
        r#"    <LIEN_ART id="LEGIARTI000000000001" typelien="CITATION"/>
    <LIEN_SECTION_TA id="LEGISCTA000000000002" typelien="CITATION"/>
    <LIEN_TXT id="LEGITEXT000000000003" typelien="CITATION">Texte cible</LIEN_TXT>
  </LIENS>"#,
    );
    let document = parse_article_fixture(&xml).unwrap();

    for (tag, target) in [
        ("LIEN", "LEGIARTI000006554637"),
        ("LIEN_ART", "LEGIARTI000000000001"),
        ("LIEN_SECTION_TA", "LEGISCTA000000000002"),
        ("LIEN_TXT", "LEGITEXT000000000003"),
    ] {
        let edge = document
            .publisher_edges
            .iter()
            .find(|edge| edge.source_tag == tag)
            .unwrap_or_else(|| panic!("missing {tag} publisher edge"));
        assert_eq!(edge.to_source_uid.as_deref(), Some(target));
        assert_eq!(edge.edge_source, "publisher");
    }
}

#[test]
fn source_uid_extraction_requires_twelve_digits_after_known_prefix() {
    assert_eq!(
        extract_known_source_uid("/codes/article_lc/LEGIARTI000006419321X"),
        Some("LEGIARTI000006419321".to_owned())
    );
    assert_eq!(extract_known_source_uid("LEGIARTI00000641932X"), None);
}

#[test]
fn parse_member_uses_raw_archive_member_hash_and_provenance() {
    let member = ArchiveMember {
        archive_path: PathBuf::from("/tmp/Freemium_legi_global.tar.gz"),
        member_path: "legi/articles/LEGIARTI000006419320.xml".to_owned(),
        bytes: article_fixture().into_bytes(),
    };

    let document = match parse_legi_member(&member).unwrap() {
        ParsedLegiXml::Article(document) => *document,
        ParsedLegiXml::UnsupportedRoot { root } => {
            panic!("expected article, got unsupported root {root}")
        }
        other => {
            panic!("expected article, got {} root", other.root_name())
        }
    };

    assert_eq!(
        document.source_archive.as_deref(),
        Some("Freemium_legi_global.tar.gz")
    );
    assert_eq!(
        document.source_member_path.as_deref(),
        Some("legi/articles/LEGIARTI000006419320.xml")
    );
    assert_eq!(
        document.source_payload_hash,
        source_payload_hash(&member.bytes)
    );
}

#[test]
fn rootless_archive_members_are_skipped_as_empty_xml() {
    for bytes in [
        b" \n\t ".as_slice(),
        b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        b"<!-- placeholder -->\n",
        b"\xEF\xBB\xBF",
    ] {
        let member = ArchiveMember {
            archive_path: PathBuf::from("/tmp/Freemium_legi_global.tar.gz"),
            member_path: "legi/global/eli/example/versions.xml".to_owned(),
            bytes: bytes.to_vec(),
        };

        let parsed = parse_legi_member(&member).unwrap();

        assert_eq!(
            parsed,
            ParsedLegiXml::UnsupportedRoot {
                root: "EMPTY_XML".to_owned()
            }
        );
    }
}

#[test]
fn provenance_from_member_handles_missing_archive_file_name() {
    let member = ArchiveMember {
        archive_path: PathBuf::new(),
        member_path: "legi/articles/LEGIARTI000006419320.xml".to_owned(),
        bytes: article_fixture().into_bytes(),
    };

    let provenance = SourceProvenance::from_archive_member(&member);

    assert_eq!(provenance.archive_name, None);
    assert_eq!(
        provenance.member_path.as_deref(),
        Some("legi/articles/LEGIARTI000006419320.xml")
    );
    assert_eq!(
        provenance.payload_hash.as_deref(),
        Some(source_payload_hash(&member.bytes).as_str())
    );
}

#[test]
fn accepts_articles_without_optional_status() {
    let document =
        parse_article_fixture(&article_fixture().replace("      <ETAT>VIGUEUR</ETAT>\n", ""))
            .unwrap();

    assert_eq!(document.source_status, None);
    assert!(document.canonical_version.contains("etat=absent"));
    assert!(document.validate().is_ok());
}

#[test]
fn accepts_empty_status_elements_as_absent() {
    for xml in [
        article_fixture().replace("<ETAT>VIGUEUR</ETAT>", "<ETAT></ETAT>"),
        article_fixture().replace("<ETAT>VIGUEUR</ETAT>", "<ETAT/>"),
    ] {
        let document = parse_article_fixture(&xml).unwrap();

        assert_eq!(document.source_status, None);
        assert!(document.canonical_version.contains("etat=absent"));
        assert!(document.validate().is_ok());
    }
}

#[test]
fn accepts_articles_without_num_or_type_as_absent_metadata() {
    let cases = [
        (
            "missing NUM",
            article_fixture().replace("      <NUM>1240</NUM>\n", ""),
            Some("Article LEGIARTI000006419320"),
            Some("AUTONOME"),
        ),
        (
            "empty NUM",
            article_fixture().replace("<NUM>1240</NUM>", "<NUM></NUM>"),
            Some("Article LEGIARTI000006419320"),
            Some("AUTONOME"),
        ),
        (
            "missing TYPE",
            article_fixture().replace("      <TYPE>AUTONOME</TYPE>\n", ""),
            Some("Article 1240"),
            None,
        ),
        (
            "empty TYPE",
            article_fixture().replace("<TYPE>AUTONOME</TYPE>", "<TYPE/>"),
            Some("Article 1240"),
            None,
        ),
        (
            "missing NUM and TYPE",
            article_fixture()
                .replace("      <NUM>1240</NUM>\n", "")
                .replace("      <TYPE>AUTONOME</TYPE>\n", ""),
            Some("Article LEGIARTI000006419320"),
            None,
        ),
    ];

    for (name, xml, expected_title, expected_type) in cases {
        let document = parse_article_fixture(&xml).unwrap_or_else(|error| {
            panic!("{name} should parse with absent display metadata: {error}")
        });

        assert_eq!(document.title.as_deref(), expected_title, "{name}");
        assert_eq!(
            document.source_article_type.as_deref(),
            expected_type,
            "{name}"
        );
        assert_eq!(
            document.canonical_version.contains("type=absent"),
            expected_type.is_none(),
            "{name}"
        );
        if expected_title == Some("Article LEGIARTI000006419320") {
            assert!(
                document.chunks.first().is_some_and(|chunk| chunk
                    .contextualized_body
                    .contains("Article LEGIARTI000006419320\n\n")),
                "{name}"
            );
        }
        assert!(document.validate().is_ok(), "{name}");
    }
}

#[test]
fn rejects_articles_without_body_content() {
    let error = parse_article_fixture(&article_fixture().replace(
        r#"  <BLOC_TEXTUEL>
    <CONTENU>
      <p>Tout fait quelconque de l'homme, qui cause a autrui un dommage, oblige celui par la faute duquel il est arrive a le reparer.</p>
    </CONTENU>
  </BLOC_TEXTUEL>
"#,
        "",
    ))
    .unwrap_err();

    assert!(matches!(
        error,
        LegiParseError::MissingRequiredField {
            field: "BLOC_TEXTUEL/CONTENU",
            ..
        }
    ));
}

#[test]
fn parses_text_version_metadata_root() {
    let parsed = parse_legi_xml(
        r#"
<TEXTE_VERSION>
  <META>
    <META_COMMUN>
      <ID>LEGITEXT000006070721</ID>
      <URL>/codes/texte_lc/LEGITEXT000006070721</URL>
      <NATURE>CODE</NATURE>
    </META_COMMUN>
    <META_SPEC>
      <META_TEXTE_VERSION>
        <TITRE>Code civil</TITRE>
        <TITREFULL>Code civil complet</TITREFULL>
        <ETAT>VIGUEUR</ETAT>
        <DATE_DEBUT>2024-01-01</DATE_DEBUT>
        <DATE_FIN>2999-01-01</DATE_FIN>
      </META_TEXTE_VERSION>
    </META_SPEC>
  </META>
</TEXTE_VERSION>
"#,
        provenance(),
    )
    .unwrap();

    let ParsedLegiXml::TextVersion(text) = parsed else {
        panic!("expected TEXTE_VERSION metadata root");
    };
    assert_eq!(text.text_id, "LEGITEXT000006070721");
    assert_eq!(text.title, "Code civil");
    assert_eq!(text.title_full.as_deref(), Some("Code civil complet"));
    assert_eq!(text.nature.as_deref(), Some("CODE"));
    assert_eq!(text.valid_from, "2024-01-01");
    assert_eq!(text.valid_to, None);
    assert_eq!(text.valid_to_raw.as_deref(), Some("2999-01-01"));
    assert_eq!(
        text.source_archive.as_deref(),
        Some("Freemium_legi_global.tar.gz")
    );
    assert!(text.source_payload_hash.starts_with("sha256:"));
}

#[test]
fn parses_text_version_with_empty_nature_as_absent() {
    let parsed = parse_legi_xml(
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
        provenance(),
    )
    .unwrap();

    let ParsedLegiXml::TextVersion(text) = parsed else {
        panic!("expected TEXTE_VERSION metadata root");
    };
    assert_eq!(text.text_id, "LEGITEXT000049371154");
    assert_eq!(text.nature, None);
    assert_eq!(text.canonical_version, "legi_text_version:v1:nature=absent");
    assert_eq!(text.valid_from, "1956-04-12");
    assert_eq!(text.valid_to, None);
}

#[test]
fn parses_section_ta_metadata_root_with_context() {
    let parsed = parse_legi_xml(
        r#"
<SECTION_TA>
  <ID>LEGISCTA000006089696</ID>
  <TITRE_TA>Titre preliminaire</TITRE_TA>
  <CONTEXTE>
    <TEXTE cid="LEGITEXT000006070721">
      <TITRE_TXT debut="1804-03-21" fin="2999-01-01">Code civil</TITRE_TXT>
      <TM><TITRE_TM>Livre Ier</TITRE_TM></TM>
    </TEXTE>
  </CONTEXTE>
</SECTION_TA>
"#,
        provenance(),
    )
    .unwrap();

    let ParsedLegiXml::SectionTa(section) = parsed else {
        panic!("expected SECTION_TA metadata root");
    };
    assert_eq!(section.section_id.as_deref(), Some("LEGISCTA000006089696"));
    assert_eq!(section.title, "Titre preliminaire");
    assert_eq!(section.valid_from, "1804-03-21");
    assert_eq!(section.valid_to, None);
    assert_eq!(
        section.parent_text_id.as_deref(),
        Some("LEGITEXT000006070721")
    );
    assert_eq!(
        section.hierarchy_path,
        vec!["Code civil".to_owned(), "Livre Ier".to_owned()]
    );
}

#[test]
fn parses_textelr_metadata_root_with_date_hint() {
    let parsed = parse_legi_xml(
        r#"
<TEXTELR>
  <META>
    <META_COMMUN>
      <ID>LEGITEXT000006070721</ID>
      <URL>/codes/texte_lc/LEGITEXT000006070721</URL>
      <NATURE>CODE</NATURE>
    </META_COMMUN>
    <META_SPEC>
      <META_TEXTE_CHRONICLE>
        <CID>LEGITEXT000006070721</CID>
        <NUM>1</NUM>
        <NOR>NOR0000000001A</NOR>
        <DATE_PUBLI>1804-03-21</DATE_PUBLI>
        <DATE_TEXTE>1804-03-21</DATE_TEXTE>
      </META_TEXTE_CHRONICLE>
    </META_SPEC>
  </META>
  <STRUCT>
    <LIEN_TXT cid="LEGITEXT999999999999" debut="1804-03-21" id="LEGITEXT000000000001">Texte lie</LIEN_TXT>
    <LIEN_ART debut="1804-02-21" etat="VIGUEUR" fin="2999-01-01" id="LEGIARTI000006419320" num="1" origine="LEGI"/>
    <LIEN_SECTION_TA cid="LEGISCTA000006089696" debut="1804-03-21" etat="VIGUEUR" fin="2999-01-01" id="LEGISCTA000006089696" niv="1" url="/codes/section_lc/LEGISCTA000006089696">Titre preliminaire</LIEN_SECTION_TA>
  </STRUCT>
</TEXTELR>
"#,
        provenance(),
    )
    .unwrap();

    let ParsedLegiXml::TextStruct(text_struct) = parsed else {
        panic!("expected TEXTELR metadata root");
    };
    assert_eq!(text_struct.text_id, "LEGITEXT000006070721");
    assert_eq!(text_struct.nature.as_deref(), Some("CODE"));
    assert_eq!(
        text_struct.source_date_debut_hint.as_deref(),
        Some("1804-02-21")
    );
    assert_eq!(text_struct.date_publi.as_deref(), Some("1804-03-21"));
    assert_eq!(text_struct.date_texte.as_deref(), Some("1804-03-21"));
    assert_eq!(text_struct.structure_links.len(), 3);
    assert_eq!(text_struct.structure_links[0].source_tag, "LIEN_TXT");
    assert_eq!(text_struct.structure_links[0].order, 0);
    assert_eq!(
        text_struct.structure_links[0].target_source_uid.as_deref(),
        Some("LEGITEXT000000000001")
    );
    assert_eq!(
        text_struct.structure_links[0].text.as_deref(),
        Some("Texte lie")
    );
    assert_eq!(text_struct.structure_links[1].source_tag, "LIEN_ART");
    assert_eq!(text_struct.structure_links[1].order, 1);
    assert_eq!(
        text_struct.structure_links[1].target_source_uid.as_deref(),
        Some("LEGIARTI000006419320")
    );
    assert_eq!(
        text_struct.structure_links[1].debut.as_deref(),
        Some("1804-02-21")
    );
    assert_eq!(
        text_struct.structure_links[1].fin.as_deref(),
        Some("2999-01-01")
    );
    assert_eq!(text_struct.structure_links[2].source_tag, "LIEN_SECTION_TA");
    assert_eq!(text_struct.structure_links[2].order, 2);
    assert_eq!(
        text_struct.structure_links[2].target_source_uid.as_deref(),
        Some("LEGISCTA000006089696")
    );
    assert_eq!(text_struct.structure_links[2].level, Some(1));
    assert_eq!(
        text_struct.structure_links[2].text.as_deref(),
        Some("Titre preliminaire")
    );
}

#[test]
fn rejects_missing_required_fields() {
    let error = parse_article_fixture(
        r#"<ARTICLE><META><META_COMMUN><ID>LEGIARTI000006419320</ID></META_COMMUN></META></ARTICLE>"#,
    )
    .unwrap_err();

    assert!(matches!(
        error,
        LegiParseError::MissingRequiredField {
            field: "META_COMMUN/NATURE",
            ..
        }
    ));
}

#[test]
fn rejects_invalid_dates() {
    let error =
        parse_article_fixture(&article_fixture().replace("1804-02-21", "1804-99-21")).unwrap_err();

    assert!(matches!(
        error,
        LegiParseError::InvalidDate {
            field: "META_ARTICLE/DATE_DEBUT",
            ..
        }
    ));
}

#[test]
fn rejects_invalid_article_ids() {
    let error = parse_article_fixture(&article_fixture().replace("LEGIARTI000006419320", "BAD"))
        .unwrap_err();

    assert!(matches!(
        error,
        LegiParseError::InvalidId {
            field: "META_COMMUN/ID",
            ..
        }
    ));
}

#[test]
fn classifies_unsupported_roots() {
    let parsed = parse_legi_xml(
        "<TEXTEKALI><META><META_COMMUN><ID>KALITEXT000005652781</ID></META_COMMUN></META></TEXTEKALI>",
        provenance(),
    )
    .unwrap();

    assert_eq!(
        parsed,
        ParsedLegiXml::UnsupportedRoot {
            root: "TEXTEKALI".to_owned()
        }
    );
}

fn parse_article_fixture(xml: &str) -> Result<CanonicalDocument, LegiParseError> {
    match parse_legi_xml(xml, provenance())? {
        ParsedLegiXml::Article(document) => Ok(*document),
        ParsedLegiXml::UnsupportedRoot { root } => {
            panic!("expected article, got unsupported root {root}")
        }
        other => {
            panic!("expected article, got {} root", other.root_name())
        }
    }
}

fn provenance() -> SourceProvenance {
    SourceProvenance {
        archive_name: Some("Freemium_legi_global.tar.gz".to_owned()),
        member_path: Some("legi/articles/LEGIARTI.xml".to_owned()),
        payload_hash: None,
    }
}

fn article_fixture() -> String {
    r#"
<ARTICLE>
  <META>
    <META_COMMUN>
      <ID>LEGIARTI000006419320</ID>
      <URL>/codes/article_lc/LEGIARTI000006419320</URL>
      <NATURE>Article</NATURE>
    </META_COMMUN>
    <META_ARTICLE>
      <NUM>1240</NUM>
      <ETAT>VIGUEUR</ETAT>
      <TYPE>AUTONOME</TYPE>
      <DATE_DEBUT>1804-02-21</DATE_DEBUT>
      <DATE_FIN>2999-01-01</DATE_FIN>
    </META_ARTICLE>
  </META>
  <CONTEXTE>
    <TEXTE>
      <TITRE_TXT>Code civil</TITRE_TXT>
      <TM>
        <TITRE_TM>Livre III : Des differentes manieres dont on acquiert la propriete</TITRE_TM>
        <TM>
          <TITRE_TM>Titre IV : Des engagements qui se forment sans convention</TITRE_TM>
        </TM>
      </TM>
    </TEXTE>
  </CONTEXTE>
  <BLOC_TEXTUEL>
    <CONTENU>
      <p>Tout fait quelconque de l'homme, qui cause a autrui un dommage, oblige celui par la faute duquel il est arrive a le reparer.</p>
    </CONTENU>
  </BLOC_TEXTUEL>
  <LIENS>
    <LIEN cidtexte="JORFTEXT000000696195" id="LEGIARTI000006554637" sens="cible" typelien="MODIFICATION">Decret no 73-138 - art. 11</LIEN>
  </LIENS>
</ARTICLE>
"#
    .to_owned()
}
