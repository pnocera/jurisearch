//! LEGI XML parser: root detection, article/text-version/section/text-struct parsing, raw nodes.

use super::*;

const LEGI_EMPTY_XML_ROOT: &str = "EMPTY_XML";

#[derive(Debug, Default)]
pub(super) struct RawArticle {
    id: Option<String>,
    url: Option<String>,
    nature: Option<String>,
    etat: Option<String>,
    num: Option<String>,
    article_type: Option<String>,
    date_debut: Option<String>,
    date_fin: Option<String>,
    body: String,
    hierarchy_path: Vec<String>,
    pub(super) publisher_links: Vec<RawPublisherLink>,
}

#[derive(Debug, Default)]
struct RawTextVersion {
    id: Option<String>,
    url: Option<String>,
    nature: Option<String>,
    title: Option<String>,
    title_full: Option<String>,
    status: Option<String>,
    date_debut: Option<String>,
    date_fin: Option<String>,
}

#[derive(Debug, Default)]
struct RawSectionTa {
    id: Option<String>,
    title: Option<String>,
    date_debut: Option<String>,
    date_fin: Option<String>,
    parent_text_id: Option<String>,
    hierarchy_path: Vec<String>,
}

#[derive(Debug, Default)]
pub(super) struct RawTextStruct {
    id: Option<String>,
    url: Option<String>,
    nature: Option<String>,
    cid: Option<String>,
    num: Option<String>,
    nor: Option<String>,
    date_publi: Option<String>,
    date_texte: Option<String>,
    source_date_debut_hint: Option<String>,
    pub(super) structure_links: Vec<ParsedTextStructLink>,
}

pub fn parse_legi_xml(
    xml: &str,
    provenance: SourceProvenance,
) -> Result<ParsedLegiXml, LegiParseError> {
    let root = detect_root(xml)?;
    match root.as_str() {
        "ARTICLE" => parse_article(xml, provenance)
            .map(Box::new)
            .map(ParsedLegiXml::Article),
        "TEXTE_VERSION" => parse_text_version(xml, provenance)
            .map(Box::new)
            .map(ParsedLegiXml::TextVersion),
        "SECTION_TA" => parse_section_ta(xml, provenance)
            .map(Box::new)
            .map(ParsedLegiXml::SectionTa),
        "TEXTELR" => parse_text_struct(xml, provenance)
            .map(Box::new)
            .map(ParsedLegiXml::TextStruct),
        _ => Ok(ParsedLegiXml::UnsupportedRoot { root }),
    }
}

pub fn parse_legi_member(member: &ArchiveMember) -> Result<ParsedLegiXml, LegiParseError> {
    let xml = std::str::from_utf8(&member.bytes).map_err(|error| LegiParseError::Xml {
        message: format!(
            "archive member `{}` is not valid UTF-8 XML: {error}",
            member.member_path
        ),
    })?;
    parse_legi_xml(xml, SourceProvenance::from_archive_member(member))
}

fn detect_root(xml: &str) -> Result<String, LegiParseError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) | Ok(Event::Empty(start)) => {
                return Ok(local_name(start.local_name().as_ref()));
            }
            Ok(Event::Eof) => {
                return Ok(LEGI_EMPTY_XML_ROOT.to_owned());
            }
            Ok(_) => {}
            Err(error) => {
                return Err(LegiParseError::Xml {
                    message: error.to_string(),
                });
            }
        }
    }
}

fn parse_article(
    xml: &str,
    provenance: SourceProvenance,
) -> Result<CanonicalDocument, LegiParseError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::<String>::new();
    let mut link_stack = Vec::<usize>::new();
    let mut raw = RawArticle::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let name = local_name(start.local_name().as_ref());
                let link_index = if is_publisher_link_tag(name.as_str()) {
                    Some(push_publisher_link(&mut raw, &start, name.as_str())?)
                } else {
                    None
                };
                stack.push(name);
                if let Some(link_index) = link_index {
                    link_stack.push(link_index);
                }
            }
            Ok(Event::Empty(start)) => {
                let name = local_name(start.local_name().as_ref());
                if is_publisher_link_tag(name.as_str()) {
                    push_publisher_link(&mut raw, &start, name.as_str())?;
                }
                stack.push(name);
                append_body_block_boundary_for_current_tag(&mut raw, &stack);
                stack.pop();
            }
            Ok(Event::End(_)) => {
                append_body_block_boundary_for_current_tag(&mut raw, &stack);
                if stack
                    .last()
                    .is_some_and(|name| is_publisher_link_tag(name.as_str()))
                {
                    link_stack.pop();
                }
                stack.pop();
            }
            Ok(Event::Text(text)) => {
                let value = text.decode().map_err(|error| LegiParseError::Xml {
                    message: error.to_string(),
                })?;
                assign_article_text(&mut raw, &stack, value.as_ref());
                assign_link_text(&mut raw, &link_stack, value.as_ref());
            }
            Ok(Event::CData(text)) => {
                let value = String::from_utf8_lossy(text.as_ref());
                assign_article_text(&mut raw, &stack, value.as_ref());
                assign_link_text(&mut raw, &link_stack, value.as_ref());
            }
            Ok(Event::GeneralRef(reference)) => {
                let value = resolve_reference(&reference)?;
                assign_article_text(&mut raw, &stack, value.as_str());
                assign_link_text(&mut raw, &link_stack, value.as_str());
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(LegiParseError::Xml {
                    message: error.to_string(),
                });
            }
        }
    }

    raw.into_document(xml, provenance)
}

fn parse_text_version(
    xml: &str,
    provenance: SourceProvenance,
) -> Result<ParsedTextVersion, LegiParseError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::<String>::new();
    let mut raw = RawTextVersion::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                stack.push(local_name(start.local_name().as_ref()));
            }
            Ok(Event::Empty(start)) => {
                stack.push(local_name(start.local_name().as_ref()));
                stack.pop();
            }
            Ok(Event::End(_)) => {
                stack.pop();
            }
            Ok(Event::Text(text)) => {
                let value = text.decode().map_err(|error| LegiParseError::Xml {
                    message: error.to_string(),
                })?;
                assign_text_version_text(&mut raw, &stack, value.as_ref());
            }
            Ok(Event::CData(text)) => {
                let value = String::from_utf8_lossy(text.as_ref());
                assign_text_version_text(&mut raw, &stack, value.as_ref());
            }
            Ok(Event::GeneralRef(reference)) => {
                let value = resolve_reference(&reference)?;
                assign_text_version_text(&mut raw, &stack, value.as_str());
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(LegiParseError::Xml {
                    message: error.to_string(),
                });
            }
        }
    }

    raw.into_text_version(xml, provenance)
}

fn parse_section_ta(
    xml: &str,
    provenance: SourceProvenance,
) -> Result<ParsedSectionTa, LegiParseError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::<String>::new();
    let mut raw = RawSectionTa::default();
    let mut in_contexte = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let name = local_name(start.local_name().as_ref());
                if name == "CONTEXTE" {
                    in_contexte = true;
                } else if in_contexte && name == "TEXTE" && raw.parent_text_id.is_none() {
                    raw.parent_text_id = attribute_value(&start, "cid")?
                        .and_then(|value| optional_non_empty(Some(value)));
                } else if in_contexte && name == "TITRE_TXT" {
                    assign_section_title_dates(&mut raw, &start)?;
                }
                stack.push(name);
            }
            Ok(Event::Empty(start)) => {
                let name = local_name(start.local_name().as_ref());
                if in_contexte && name == "TEXTE" && raw.parent_text_id.is_none() {
                    raw.parent_text_id = attribute_value(&start, "cid")?
                        .and_then(|value| optional_non_empty(Some(value)));
                } else if in_contexte && name == "TITRE_TXT" {
                    assign_section_title_dates(&mut raw, &start)?;
                }
                stack.push(name);
                stack.pop();
            }
            Ok(Event::End(_)) => {
                if stack.last().is_some_and(|name| name == "CONTEXTE") {
                    in_contexte = false;
                }
                stack.pop();
            }
            Ok(Event::Text(text)) => {
                let value = text.decode().map_err(|error| LegiParseError::Xml {
                    message: error.to_string(),
                })?;
                assign_section_text(&mut raw, &stack, value.as_ref());
            }
            Ok(Event::CData(text)) => {
                let value = String::from_utf8_lossy(text.as_ref());
                assign_section_text(&mut raw, &stack, value.as_ref());
            }
            Ok(Event::GeneralRef(reference)) => {
                let value = resolve_reference(&reference)?;
                assign_section_text(&mut raw, &stack, value.as_str());
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(LegiParseError::Xml {
                    message: error.to_string(),
                });
            }
        }
    }

    raw.into_section_ta(xml, provenance)
}

fn parse_text_struct(
    xml: &str,
    provenance: SourceProvenance,
) -> Result<ParsedTextStruct, LegiParseError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::<String>::new();
    let mut link_stack = Vec::<usize>::new();
    let mut raw = RawTextStruct::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let name = local_name(start.local_name().as_ref());
                let link_index = if is_text_struct_link_tag(name.as_str()) {
                    Some(push_text_struct_link(&mut raw, &start, name.as_str())?)
                } else {
                    None
                };
                if is_text_struct_link_tag(name.as_str()) {
                    assign_text_struct_date_hint(&mut raw, &start)?;
                }
                stack.push(name);
                if let Some(link_index) = link_index {
                    link_stack.push(link_index);
                }
            }
            Ok(Event::Empty(start)) => {
                let name = local_name(start.local_name().as_ref());
                if is_text_struct_link_tag(name.as_str()) {
                    push_text_struct_link(&mut raw, &start, name.as_str())?;
                }
                if is_text_struct_link_tag(name.as_str()) {
                    assign_text_struct_date_hint(&mut raw, &start)?;
                }
                stack.push(name);
                stack.pop();
            }
            Ok(Event::End(_)) => {
                if stack
                    .last()
                    .is_some_and(|name| is_text_struct_link_tag(name.as_str()))
                {
                    link_stack.pop();
                }
                stack.pop();
            }
            Ok(Event::Text(text)) => {
                let value = text.decode().map_err(|error| LegiParseError::Xml {
                    message: error.to_string(),
                })?;
                assign_text_struct_text(&mut raw, &stack, value.as_ref());
                assign_text_struct_link_text(&mut raw, &link_stack, value.as_ref());
            }
            Ok(Event::CData(text)) => {
                let value = String::from_utf8_lossy(text.as_ref());
                assign_text_struct_text(&mut raw, &stack, value.as_ref());
                assign_text_struct_link_text(&mut raw, &link_stack, value.as_ref());
            }
            Ok(Event::GeneralRef(reference)) => {
                let value = resolve_reference(&reference)?;
                assign_text_struct_text(&mut raw, &stack, value.as_str());
                assign_text_struct_link_text(&mut raw, &link_stack, value.as_str());
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(LegiParseError::Xml {
                    message: error.to_string(),
                });
            }
        }
    }

    raw.into_text_struct(xml, provenance)
}

fn assign_article_text(raw: &mut RawArticle, stack: &[String], value: &str) {
    if path_contains(stack, &["BLOC_TEXTUEL", "CONTENU"]) {
        append_xml_content(&mut raw.body, value);
        return;
    }

    if value.trim().is_empty() {
        return;
    }
    let trimmed = value.trim();

    if path_ends_with(stack, &["META_COMMUN", "ID"]) {
        assign_if_empty(&mut raw.id, trimmed);
    } else if path_ends_with(stack, &["META_COMMUN", "URL"]) {
        assign_if_empty(&mut raw.url, trimmed);
    } else if path_ends_with(stack, &["META_COMMUN", "NATURE"]) {
        assign_if_empty(&mut raw.nature, trimmed);
    } else if path_ends_with(stack, &["META_ARTICLE", "ETAT"]) {
        assign_if_empty(&mut raw.etat, trimmed);
    } else if path_ends_with(stack, &["META_ARTICLE", "NUM"]) {
        assign_if_empty(&mut raw.num, trimmed);
    } else if path_ends_with(stack, &["META_ARTICLE", "TYPE"]) {
        assign_if_empty(&mut raw.article_type, trimmed);
    } else if path_ends_with(stack, &["META_ARTICLE", "DATE_DEBUT"]) {
        assign_if_empty(&mut raw.date_debut, trimmed);
    } else if path_ends_with(stack, &["META_ARTICLE", "DATE_FIN"]) {
        assign_if_empty(&mut raw.date_fin, trimmed);
    } else if path_contains(stack, &["CONTEXTE"])
        && (path_ends_with(stack, &["TITRE_TXT"]) || path_ends_with(stack, &["TITRE_TM"]))
    {
        raw.hierarchy_path.push(trimmed.to_owned());
    }
}

fn assign_text_version_text(raw: &mut RawTextVersion, stack: &[String], value: &str) {
    if value.trim().is_empty() {
        return;
    }
    let trimmed = value.trim();

    if path_ends_with(stack, &["META_COMMUN", "ID"]) {
        assign_if_empty(&mut raw.id, trimmed);
    } else if path_ends_with(stack, &["META_COMMUN", "URL"]) {
        assign_if_empty(&mut raw.url, trimmed);
    } else if path_ends_with(stack, &["META_COMMUN", "NATURE"]) {
        assign_if_empty(&mut raw.nature, trimmed);
    } else if path_ends_with(stack, &["META_TEXTE_VERSION", "TITRE"]) {
        assign_if_empty(&mut raw.title, trimmed);
    } else if path_ends_with(stack, &["META_TEXTE_VERSION", "TITREFULL"]) {
        assign_if_empty(&mut raw.title_full, trimmed);
    } else if path_ends_with(stack, &["META_TEXTE_VERSION", "ETAT"]) {
        assign_if_empty(&mut raw.status, trimmed);
    } else if path_ends_with(stack, &["META_TEXTE_VERSION", "DATE_DEBUT"]) {
        assign_if_empty(&mut raw.date_debut, trimmed);
    } else if path_ends_with(stack, &["META_TEXTE_VERSION", "DATE_FIN"]) {
        assign_if_empty(&mut raw.date_fin, trimmed);
    }
}

fn assign_section_text(raw: &mut RawSectionTa, stack: &[String], value: &str) {
    if value.trim().is_empty() {
        return;
    }
    let trimmed = value.trim();

    if path_ends_with(stack, &["SECTION_TA", "ID"]) {
        assign_if_empty(&mut raw.id, trimmed);
    } else if path_ends_with(stack, &["SECTION_TA", "TITRE_TA"]) {
        assign_if_empty(&mut raw.title, trimmed);
    } else if path_contains(stack, &["CONTEXTE"])
        && (path_ends_with(stack, &["TITRE_TXT"]) || path_ends_with(stack, &["TITRE_TM"]))
    {
        raw.hierarchy_path.push(trimmed.to_owned());
    }
}

fn assign_section_title_dates(
    raw: &mut RawSectionTa,
    start: &BytesStart<'_>,
) -> Result<(), LegiParseError> {
    if let Some(debut) = attribute_value(start, "debut")?
        && !debut.trim().is_empty()
    {
        raw.date_debut = Some(debut);
    }
    if let Some(fin) = attribute_value(start, "fin")?
        && !fin.trim().is_empty()
    {
        raw.date_fin = Some(fin);
    }
    Ok(())
}

fn assign_text_struct_text(raw: &mut RawTextStruct, stack: &[String], value: &str) {
    if value.trim().is_empty() {
        return;
    }
    let trimmed = value.trim();

    if path_ends_with(stack, &["META_COMMUN", "ID"]) {
        assign_if_empty(&mut raw.id, trimmed);
    } else if path_ends_with(stack, &["META_COMMUN", "URL"]) {
        assign_if_empty(&mut raw.url, trimmed);
    } else if path_ends_with(stack, &["META_COMMUN", "NATURE"]) {
        assign_if_empty(&mut raw.nature, trimmed);
    } else if path_ends_with(stack, &["META_TEXTE_CHRONICLE", "CID"]) {
        assign_if_empty(&mut raw.cid, trimmed);
    } else if path_ends_with(stack, &["META_TEXTE_CHRONICLE", "NUM"]) {
        assign_if_empty(&mut raw.num, trimmed);
    } else if path_ends_with(stack, &["META_TEXTE_CHRONICLE", "NOR"]) {
        assign_if_empty(&mut raw.nor, trimmed);
    } else if path_ends_with(stack, &["META_TEXTE_CHRONICLE", "DATE_PUBLI"]) {
        assign_if_empty(&mut raw.date_publi, trimmed);
    } else if path_ends_with(stack, &["META_TEXTE_CHRONICLE", "DATE_TEXTE"]) {
        assign_if_empty(&mut raw.date_texte, trimmed);
    }
}

fn assign_text_struct_date_hint(
    raw: &mut RawTextStruct,
    start: &BytesStart<'_>,
) -> Result<(), LegiParseError> {
    let Some(debut) = attribute_value(start, "debut")? else {
        return Ok(());
    };
    let Some(debut) = optional_non_empty(Some(debut)) else {
        return Ok(());
    };
    validate_date("LIEN@debut", debut.as_str())?;
    match &raw.source_date_debut_hint {
        Some(current) if current <= &debut => {}
        _ => raw.source_date_debut_hint = Some(debut),
    }
    Ok(())
}

fn append_body_block_boundary_for_current_tag(raw: &mut RawArticle, stack: &[String]) {
    if stack
        .last()
        .is_some_and(|name| is_body_block_boundary(name.as_str()))
        && path_contains(stack, &["BLOC_TEXTUEL", "CONTENU"])
    {
        append_block_boundary(&mut raw.body);
    }
}

impl RawArticle {
    fn into_document(
        self,
        xml: &str,
        provenance: SourceProvenance,
    ) -> Result<CanonicalDocument, LegiParseError> {
        let id = required("article", "META_COMMUN/ID", self.id)?;
        validate_id("META_COMMUN/ID", &id, "LEGIARTI", "LEGIARTI[0-9]{12}")?;
        let nature = required("article", "META_COMMUN/NATURE", self.nature)?;
        let etat = optional_non_empty(self.etat);
        let num = optional_non_empty(self.num);
        let article_type = optional_non_empty(self.article_type);
        let valid_from = normalize_required_date(
            "META_ARTICLE/DATE_DEBUT",
            &required("article", "META_ARTICLE/DATE_DEBUT", self.date_debut)?,
        )?;
        let valid_to_raw = required("article", "META_ARTICLE/DATE_FIN", self.date_fin)?;
        let valid_to = normalize_end_date("META_ARTICLE/DATE_FIN", &valid_to_raw)?;
        let body = required_non_empty("article", "BLOC_TEXTUEL/CONTENU", self.body)?;
        let source_payload_hash = provenance
            .payload_hash
            .unwrap_or_else(|| source_payload_hash(xml.as_bytes()));
        let publisher_links = self.publisher_links;
        let title = num
            .as_deref()
            .map(|num| format!("Article {num}"))
            .unwrap_or_else(|| format!("Article {id}"));
        let citation_prefix = self
            .hierarchy_path
            .first()
            .cloned()
            .unwrap_or_else(|| "LEGI".to_owned());

        let mut document = CanonicalDocument {
            document_id: format!("legi:{id}@{valid_from}"),
            source: "legi".to_owned(),
            kind: "article".to_owned(),
            source_uid: id.clone(),
            version_group: Some(id),
            citation: Some(format!("{citation_prefix} {title}")),
            title: Some(title),
            body,
            source_status: etat.clone(),
            source_nature: Some(nature.clone()),
            source_article_type: article_type.clone(),
            valid_from,
            valid_to,
            valid_to_raw: Some(valid_to_raw),
            source_url: self.url,
            source_payload_hash,
            source_archive: provenance.archive_name,
            source_member_path: provenance.member_path,
            hierarchy_path: self.hierarchy_path,
            publisher_edges: Vec::new(),
            chunks: Vec::new(),
            canonical_version: format!(
                "legi_article:v2:nature={nature}:etat={}:type={}",
                etat.as_deref().unwrap_or("absent"),
                article_type.as_deref().unwrap_or("absent")
            ),
        };
        document.publisher_edges = publisher_links
            .into_iter()
            .enumerate()
            .map(|(index, link)| link.into_edge(index, &document))
            .collect();
        document.chunks = build_article_chunks(&document);
        document.validate().map_err(|error| LegiParseError::Xml {
            message: format!("canonical validation failed: {error}"),
        })?;
        Ok(document)
    }
}

impl RawTextVersion {
    fn into_text_version(
        self,
        xml: &str,
        provenance: SourceProvenance,
    ) -> Result<ParsedTextVersion, LegiParseError> {
        let id = required("text_version", "META_COMMUN/ID", self.id)?;
        validate_id("META_COMMUN/ID", &id, "LEGITEXT", "LEGITEXT[0-9]{12}")?;
        let nature = optional_non_empty(self.nature);
        let title = required("text_version", "META_TEXTE_VERSION/TITRE", self.title)?;
        let status = required("text_version", "META_TEXTE_VERSION/ETAT", self.status)?;
        let valid_from = normalize_required_date(
            "META_TEXTE_VERSION/DATE_DEBUT",
            &required(
                "text_version",
                "META_TEXTE_VERSION/DATE_DEBUT",
                self.date_debut,
            )?,
        )?;
        let valid_to_raw = required("text_version", "META_TEXTE_VERSION/DATE_FIN", self.date_fin)?;
        let valid_to = normalize_end_date("META_TEXTE_VERSION/DATE_FIN", &valid_to_raw)?;
        let source_payload_hash = provenance
            .payload_hash
            .unwrap_or_else(|| source_payload_hash(xml.as_bytes()));
        let canonical_version = format!(
            "legi_text_version:v1:nature={}",
            nature.as_deref().unwrap_or("absent")
        );

        Ok(ParsedTextVersion {
            text_id: id,
            title,
            title_full: optional_non_empty(self.title_full),
            status,
            nature,
            valid_from,
            valid_to,
            valid_to_raw: Some(valid_to_raw),
            source_url: optional_non_empty(self.url),
            source_payload_hash,
            source_archive: provenance.archive_name,
            source_member_path: provenance.member_path,
            canonical_version,
        })
    }
}

impl RawSectionTa {
    fn into_section_ta(
        self,
        xml: &str,
        provenance: SourceProvenance,
    ) -> Result<ParsedSectionTa, LegiParseError> {
        let section_id = optional_non_empty(self.id)
            .map(|id| {
                validate_id("SECTION_TA/ID", &id, "LEGISCTA", "LEGISCTA[0-9]{12}")?;
                Ok::<_, LegiParseError>(id)
            })
            .transpose()?;
        let title = required("section_ta", "SECTION_TA/TITRE_TA", self.title)?;
        let valid_from = normalize_required_date(
            "TITRE_TXT@debut",
            &required("section_ta", "TITRE_TXT@debut", self.date_debut)?,
        )?;
        let valid_to_raw = required("section_ta", "TITRE_TXT@fin", self.date_fin)?;
        let valid_to = normalize_end_date("TITRE_TXT@fin", &valid_to_raw)?;
        let source_payload_hash = provenance
            .payload_hash
            .unwrap_or_else(|| source_payload_hash(xml.as_bytes()));

        Ok(ParsedSectionTa {
            section_id,
            title,
            valid_from,
            valid_to,
            valid_to_raw: Some(valid_to_raw),
            parent_text_id: self.parent_text_id,
            hierarchy_path: self.hierarchy_path,
            source_payload_hash,
            source_archive: provenance.archive_name,
            source_member_path: provenance.member_path,
            canonical_version: "legi_section_ta:v1".to_owned(),
        })
    }
}

impl RawTextStruct {
    fn into_text_struct(
        self,
        xml: &str,
        provenance: SourceProvenance,
    ) -> Result<ParsedTextStruct, LegiParseError> {
        let id = required("textelr", "META_COMMUN/ID", self.id)?;
        validate_id("META_COMMUN/ID", &id, "LEGITEXT", "LEGITEXT[0-9]{12}")?;
        if let Some(date_publi) = &self.date_publi {
            validate_date("META_TEXTE_CHRONICLE/DATE_PUBLI", date_publi)?;
        }
        if let Some(date_texte) = &self.date_texte {
            validate_date("META_TEXTE_CHRONICLE/DATE_TEXTE", date_texte)?;
        }
        let source_payload_hash = provenance
            .payload_hash
            .unwrap_or_else(|| source_payload_hash(xml.as_bytes()));

        Ok(ParsedTextStruct {
            text_id: id,
            nature: optional_non_empty(self.nature),
            source_url: optional_non_empty(self.url),
            cid: optional_non_empty(self.cid),
            num: optional_non_empty(self.num),
            nor: optional_non_empty(self.nor),
            date_publi: optional_non_empty(self.date_publi),
            date_texte: optional_non_empty(self.date_texte),
            source_date_debut_hint: self.source_date_debut_hint,
            structure_links: self.structure_links,
            source_payload_hash,
            source_archive: provenance.archive_name,
            source_member_path: provenance.member_path,
            canonical_version: "legi_textelr:v2".to_owned(),
        })
    }
}
