use super::*;
use crate::archive::ArchiveSource;
use crate::legi::SourceProvenance;

fn provenance() -> SourceProvenance {
    SourceProvenance {
        archive_name: Some("Freemium_cass_global_20250713-140000.tar.gz".to_owned()),
        member_path: Some("juri/cass/.../JURITEXT000051824029.xml".to_owned()),
        payload_hash: Some("sha256:deadbeef".to_owned()),
    }
}

fn parse(source: ArchiveSource, xml: &str) -> ParsedJuriXml {
    parse_juri_xml(source, xml, provenance()).expect("parse should succeed")
}

fn decision(source: ArchiveSource, xml: &str) -> CanonicalDecision {
    match parse(source, xml) {
        ParsedJuriXml::Decision(decision) => *decision,
        other => panic!("expected decision, got {other:?}"),
    }
}

const JUDI_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<TEXTE_JURI_JUDI>
<META><META_COMMUN>
<ID>JURITEXT000051824029</ID><ANCIEN_ID/><ORIGINE>JURI</ORIGINE>
<URL>texte/juri/judi/JURI/TEXT/00/00/51/82/40/JURITEXT000051824029.xml</URL>
<NATURE>ARRET</NATURE>
</META_COMMUN><META_SPEC><META_JURI>
<TITRE>Cour de cassation, Assemblée plénière, 27 juin 2025, 22-21.812, Publié au bulletin</TITRE>
<DATE_DEC>2025-06-27</DATE_DEC><JURIDICTION>Cour de cassation</JURIDICTION>
<NUMERO>P2500683</NUMERO><SOLUTION>Cassation partielle</SOLUTION>
</META_JURI><META_JURI_JUDI>
<NUMEROS_AFFAIRES><NUMERO_AFFAIRE>22-21812</NUMERO_AFFAIRE></NUMEROS_AFFAIRES>
<PUBLI_BULL publie="oui"/><FORMATION>ASSEMBLEE_PLENIERE</FORMATION>
<ECLI>ECLI:FR:CCASS:2025:AP00683</ECLI>
</META_JURI_JUDI></META_SPEC></META>
<TEXTE><BLOC_TEXTUEL><CONTENU>LA COUR, après débats &amp; délibéré, concernant M. [T] [P] domicilié [Adresse 2],<br/>
<br/>rejette le pourvoi.</CONTENU></BLOC_TEXTUEL>
<SOMMAIRE>
<SCT ID="1" TYPE="PRINCIPAL">CONTRAT DE TRAVAIL - Rupture</SCT>
<ANA ID="1">Il résulte de l'article L. 1242-14 du code du travail que la rupture est soumise aux prescriptions des articles L. 1332-1 à L. 1332-3.</ANA>
</SOMMAIRE>
<CITATION_JP/></TEXTE>
<LIENS>
<LIEN cidtexte="" datesignatexte="" id="" naturetexte="" nortexte="" num="" numtexte="" sens="source" typelien="CITATION">Articles L. 1242-14 du code du travail.</LIEN>
</LIENS>
</TEXTE_JURI_JUDI>"#;

const ADMIN_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<TEXTE_JURI_ADMIN>
<META><META_COMMUN>
<ID>CETATEXT000051549953</ID><ANCIEN_ID>J1_L_2025_04_00024PA03561</ANCIEN_ID><ORIGINE>CETAT</ORIGINE>
<URL>texte/juri/admin/CETA/TEXT/00/00/51/54/99/CETATEXT000051549953.xml</URL>
<NATURE>Texte</NATURE>
</META_COMMUN><META_SPEC><META_JURI>
<TITRE>CAA de PARIS, 9ème chambre, 30/04/2025, 24PA03561, Inédit au recueil Lebon</TITRE>
<DATE_DEC>2025-04-30</DATE_DEC><JURIDICTION>CAA de PARIS</JURIDICTION>
<NUMERO>24PA03561</NUMERO><SOLUTION/>
</META_JURI><META_JURI_ADMIN>
<FORMATION>9ème chambre</FORMATION><TYPE_REC>excès de pouvoir</TYPE_REC>
<PUBLI_RECUEIL>C</PUBLI_RECUEIL><RAPPORTEUR>Mme Sabine BOIZOT</RAPPORTEUR>
</META_JURI_ADMIN></META_SPEC></META>
<TEXTE><BLOC_TEXTUEL><CONTENU>Vu la procédure suivante, M. A... B... a demandé l'annulation.<br/>
Considérant ce qui suit : la requête est rejetée.</CONTENU></BLOC_TEXTUEL><SOMMAIRE/></TEXTE>
<LIENS/>
</TEXTE_JURI_ADMIN>"#;

#[test]
fn parses_judicial_decision_core_fields() {
    let decision = decision(ArchiveSource::Cass, JUDI_XML);

    assert_eq!(decision.source, "cass");
    assert_eq!(decision.source_family, JuriFamily::Judicial);
    assert_eq!(decision.kind, "decision");
    assert_eq!(decision.source_uid, "JURITEXT000051824029");
    assert_eq!(decision.document_id, "cass:JURITEXT000051824029");
    assert_eq!(decision.decision_date, "2025-06-27");
    assert_eq!(decision.jurisdiction.as_deref(), Some("Cour de cassation"));
    assert_eq!(decision.number.as_deref(), Some("P2500683"));
    assert_eq!(decision.solution.as_deref(), Some("Cassation partielle"));
    assert_eq!(decision.formation.as_deref(), Some("ASSEMBLEE_PLENIERE"));
    assert_eq!(decision.nature.as_deref(), Some("ARRET"));
    assert_eq!(
        decision.ecli.as_deref(),
        Some("ECLI:FR:CCASS:2025:AP00683")
    );
    assert_eq!(decision.publication.as_deref(), Some("oui"));
    assert_eq!(decision.case_numbers, vec!["22-21812".to_owned()]);
    assert_eq!(
        decision.source_url.as_deref(),
        Some("https://www.legifrance.gouv.fr/juri/id/JURITEXT000051824029")
    );
    assert_eq!(decision.canonical_version, "juri_decision:v1");
    assert_eq!(decision.chunking_provenance, "heuristic");
    decision.validate().expect("valid decision");
}

#[test]
fn judicial_body_decodes_entities_and_preserves_pseudonymisation() {
    let decision = decision(ArchiveSource::Cass, JUDI_XML);
    // `&amp;` decoded to `&`; `<br/>` produced a paragraph boundary; blank line dropped.
    assert_eq!(
        decision.body,
        "LA COUR, après débats & délibéré, concernant M. [T] [P] domicilié [Adresse 2],\nrejette le pourvoi."
    );
    // Source pseudonymisation tokens preserved verbatim — never de-pseudonymised.
    assert!(decision.body.contains("[Adresse 2]"));
    assert!(decision.body.contains("M. [T] [P]"));
}

#[test]
fn judicial_summaries_capture_titrage_and_analysis() {
    let decision = decision(ArchiveSource::Cass, JUDI_XML);
    assert_eq!(decision.summaries.len(), 2);
    assert_eq!(decision.summaries[0].kind, "PRINCIPAL");
    assert_eq!(decision.summaries[0].text, "CONTRAT DE TRAVAIL - Rupture");
    assert_eq!(decision.summaries[1].kind, "analyse");
    assert!(decision.summaries[1].text.contains("article L. 1242-14"));
}

#[test]
fn judicial_chunks_are_summary_then_body_and_heuristic() {
    let decision = decision(ArchiveSource::Cass, JUDI_XML);
    assert_eq!(decision.chunks.len(), 2);

    let summary = &decision.chunks[0];
    assert_eq!(summary.chunk_index, 0);
    assert_eq!(summary.chunk_kind, "decision_summary");
    assert_eq!(summary.boundary, "sommaire");
    assert_eq!(summary.source_fields, vec!["TEXTE/SOMMAIRE".to_owned()]);

    let body = &decision.chunks[1];
    assert_eq!(body.chunk_index, 1);
    assert_eq!(body.chunk_kind, "decision_body");
    assert_eq!(body.boundary, "paragraph");
    assert_eq!(
        body.source_fields,
        vec!["TEXTE/BLOC_TEXTUEL/CONTENU".to_owned()]
    );

    for chunk in &decision.chunks {
        assert_eq!(chunk.chunking, "heuristic");
        assert_eq!(chunk.chunk_builder_version, "juri_decision_heuristic:v1");
        // The contextualized body prepends the decision title for retrieval context.
        assert!(chunk.contextualized_body.contains("Assemblée plénière"));
        assert!(chunk.source_payload_hash.starts_with("sha256:"));
    }
}

#[test]
fn judicial_publisher_edges_from_liens() {
    let decision = decision(ArchiveSource::Cass, JUDI_XML);
    assert_eq!(decision.publisher_edges.len(), 1);
    let edge = &decision.publisher_edges[0];
    assert_eq!(edge.edge_source, "publisher");
    assert_eq!(edge.source_tag, "LIEN");
    assert_eq!(edge.relation, "refers_to");
    assert_eq!(edge.from_document_id, "cass:JURITEXT000051824029");
    assert_eq!(edge.from_source_uid, "JURITEXT000051824029");
    assert_eq!(edge.to_source_uid, None); // id/cidtexte empty → unresolved, evidence kept
    assert_eq!(
        edge.source_text.as_deref(),
        Some("Articles L. 1242-14 du code du travail.")
    );
    // Empty-valued attributes are dropped; meaningful ones are preserved.
    assert!(
        edge.attributes
            .iter()
            .any(|attribute| attribute.key == "typelien" && attribute.value == "CITATION")
    );
    assert!(
        edge.attributes
            .iter()
            .any(|attribute| attribute.key == "sens" && attribute.value == "source")
    );
    assert!(edge.attributes.iter().all(|attribute| !attribute.value.is_empty()));
}

#[test]
fn parses_administrative_decision() {
    let decision = decision(ArchiveSource::Jade, ADMIN_XML);

    assert_eq!(decision.source, "jade");
    assert_eq!(decision.source_family, JuriFamily::Administrative);
    assert_eq!(decision.source_uid, "CETATEXT000051549953");
    assert_eq!(decision.document_id, "jade:CETATEXT000051549953");
    assert_eq!(decision.decision_date, "2025-04-30");
    assert_eq!(decision.jurisdiction.as_deref(), Some("CAA de PARIS"));
    assert_eq!(decision.number.as_deref(), Some("24PA03561"));
    assert_eq!(decision.nature.as_deref(), Some("Texte"));
    assert_eq!(decision.formation.as_deref(), Some("9ème chambre"));
    assert_eq!(decision.publication.as_deref(), Some("C")); // PUBLI_RECUEIL
    assert_eq!(decision.solution, None); // empty <SOLUTION/>
    assert_eq!(decision.ecli, None);
    assert!(decision.case_numbers.is_empty());
    assert!(decision.summaries.is_empty());
    // No SOMMAIRE → only body chunk(s).
    assert_eq!(decision.chunks.len(), 1);
    assert_eq!(decision.chunks[0].chunk_kind, "decision_body");
    assert!(decision.publisher_edges.is_empty());
    // Administrative pseudonymisation preserved.
    assert!(decision.body.contains("M. A... B..."));
    decision.validate().expect("valid administrative decision");
}

#[test]
fn unsupported_root_is_classified_not_inserted() {
    let xml = r#"<?xml version="1.0"?><TEXTE_VERSION><META/></TEXTE_VERSION>"#;
    match parse(ArchiveSource::Cass, xml) {
        ParsedJuriXml::UnsupportedRoot { root } => assert_eq!(root, "TEXTE_VERSION"),
        other => panic!("expected unsupported root, got {other:?}"),
    }
}

#[test]
fn rejects_non_jurisprudence_source() {
    let error = parse_juri_xml(ArchiveSource::Legi, JUDI_XML, provenance()).unwrap_err();
    assert!(matches!(error, JuriParseError::UnknownSource { .. }));
}

#[test]
fn rejects_invalid_source_uid() {
    let xml = JUDI_XML.replace("JURITEXT000051824029", "JURITEXT123");
    let error = parse_juri_xml(ArchiveSource::Cass, &xml, provenance()).unwrap_err();
    assert!(matches!(error, JuriParseError::InvalidId { field: "ID", .. }));
}

#[test]
fn rejects_missing_decision_date() {
    let xml = JUDI_XML.replace("<DATE_DEC>2025-06-27</DATE_DEC>", "");
    let error = parse_juri_xml(ArchiveSource::Cass, &xml, provenance()).unwrap_err();
    assert!(matches!(
        error,
        JuriParseError::MissingRequiredField {
            field: "DATE_DEC",
            ..
        }
    ));
}

#[test]
fn rejects_invalid_decision_date() {
    let xml = JUDI_XML.replace("2025-06-27", "2025-13-40");
    let error = parse_juri_xml(ArchiveSource::Cass, &xml, provenance()).unwrap_err();
    assert!(matches!(
        error,
        JuriParseError::InvalidDate {
            field: "DATE_DEC",
            ..
        }
    ));
}

#[test]
fn long_body_splits_into_multiple_heuristic_chunks() {
    // 4 paragraphs of ~2500 chars each → must split under the 6000-char budget.
    let paragraph = "a".repeat(2_500);
    let body = format!("{paragraph}<br/>{paragraph}<br/>{paragraph}<br/>{paragraph}");
    let xml = JUDI_XML.replace(
        "LA COUR, après débats &amp; délibéré, concernant M. [T] [P] domicilié [Adresse 2],<br/>\n<br/>rejette le pourvoi.",
        &body,
    );
    let decision = decision(ArchiveSource::Cass, &xml);
    let body_chunks: Vec<_> = decision
        .chunks
        .iter()
        .filter(|chunk| chunk.chunk_kind == "decision_body")
        .collect();
    assert!(
        body_chunks.len() >= 2,
        "expected the long body to split, got {} body chunks",
        body_chunks.len()
    );
    for chunk in &body_chunks {
        assert!(chunk.body.chars().count() <= JURI_DECISION_CHUNK_MAX_CHARS);
        assert_eq!(chunk.chunking, "heuristic");
    }
    decision.validate().expect("valid decision after split");
}

#[test]
fn validate_rejects_tampered_document_id() {
    let mut decision = decision(ArchiveSource::Cass, JUDI_XML);
    decision.document_id = "judilibre:JURITEXT000051824029".to_owned();
    let error = decision.validate().unwrap_err();
    assert!(matches!(
        error,
        DecisionValidationError::InvalidDocumentId { .. }
    ));
}
