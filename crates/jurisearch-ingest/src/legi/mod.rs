use std::fmt::Write as _;

use quick_xml::{
    Reader,
    events::{BytesRef, BytesStart, Event},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::archive::ArchiveMember;

const LEGI_ARTICLE_CONTEXTUALIZED_CHUNK_MAX_CHARS: usize = 6_000;
const LEGI_ARTICLE_CHUNK_BUILDER_VERSION: &str = "legi_article_structural:v2";
const LEGI_EMPTY_XML_ROOT: &str = "EMPTY_XML";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceProvenance {
    pub archive_name: Option<String>,
    pub member_path: Option<String>,
    pub payload_hash: Option<String>,
}

impl SourceProvenance {
    pub fn from_archive_member(member: &ArchiveMember) -> Self {
        let archive_name = member
            .archive_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .or_else(|| {
                let displayed = member.archive_path.display().to_string();
                if displayed.is_empty() {
                    None
                } else {
                    Some(displayed)
                }
            });

        Self {
            archive_name,
            member_path: Some(member.member_path.clone()),
            payload_hash: Some(source_payload_hash(&member.bytes)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParsedLegiXml {
    Article(Box<CanonicalDocument>),
    TextVersion(Box<ParsedTextVersion>),
    SectionTa(Box<ParsedSectionTa>),
    TextStruct(Box<ParsedTextStruct>),
    UnsupportedRoot { root: String },
}

impl ParsedLegiXml {
    pub fn root_name(&self) -> &'static str {
        match self {
            Self::Article(_) => "ARTICLE",
            Self::TextVersion(_) => "TEXTE_VERSION",
            Self::SectionTa(_) => "SECTION_TA",
            Self::TextStruct(_) => "TEXTELR",
            Self::UnsupportedRoot { .. } => "unsupported",
        }
    }

    pub fn source_uid(&self) -> Option<&str> {
        match self {
            Self::Article(document) => Some(document.source_uid.as_str()),
            Self::TextVersion(text) => Some(text.text_id.as_str()),
            Self::SectionTa(section) => section.section_id.as_deref(),
            Self::TextStruct(text_struct) => Some(text_struct.text_id.as_str()),
            Self::UnsupportedRoot { .. } => None,
        }
    }

    pub fn date_anchor(&self) -> Option<&str> {
        match self {
            Self::Article(document) => Some(document.valid_from.as_str()),
            Self::TextVersion(text) => Some(text.valid_from.as_str()),
            Self::SectionTa(section) => Some(section.valid_from.as_str()),
            Self::TextStruct(text_struct) => text_struct.source_date_debut_hint.as_deref(),
            Self::UnsupportedRoot { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedTextVersion {
    pub text_id: String,
    pub title: String,
    pub title_full: Option<String>,
    pub status: String,
    pub nature: Option<String>,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub valid_to_raw: Option<String>,
    pub source_url: Option<String>,
    pub source_payload_hash: String,
    pub source_archive: Option<String>,
    pub source_member_path: Option<String>,
    pub canonical_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedSectionTa {
    pub section_id: Option<String>,
    pub title: String,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub valid_to_raw: Option<String>,
    pub parent_text_id: Option<String>,
    pub hierarchy_path: Vec<String>,
    pub source_payload_hash: String,
    pub source_archive: Option<String>,
    pub source_member_path: Option<String>,
    pub canonical_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedTextStruct {
    pub text_id: String,
    pub nature: Option<String>,
    pub source_url: Option<String>,
    pub cid: Option<String>,
    pub num: Option<String>,
    pub nor: Option<String>,
    pub date_publi: Option<String>,
    pub date_texte: Option<String>,
    pub source_date_debut_hint: Option<String>,
    #[serde(default)]
    pub structure_links: Vec<ParsedTextStructLink>,
    pub source_payload_hash: String,
    pub source_archive: Option<String>,
    pub source_member_path: Option<String>,
    pub canonical_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedTextStructLink {
    pub source_tag: String,
    pub order: usize,
    pub target_source_uid: Option<String>,
    pub level: Option<i32>,
    /// Raw DILA `debut` attribute; consumers normalize sentinel dates when applying temporal logic.
    pub debut: Option<String>,
    /// Raw DILA `fin` attribute; consumers normalize sentinel dates when applying temporal logic.
    pub fin: Option<String>,
    pub text: Option<String>,
    pub attributes: Vec<GraphEdgeAttribute>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalDocument {
    pub document_id: String,
    pub source: String,
    pub kind: String,
    pub source_uid: String,
    pub version_group: Option<String>,
    pub citation: Option<String>,
    pub title: Option<String>,
    pub body: String,
    pub source_status: Option<String>,
    pub source_nature: Option<String>,
    pub source_article_type: Option<String>,
    pub valid_from: String,
    pub valid_to: Option<String>,
    pub valid_to_raw: Option<String>,
    pub source_url: Option<String>,
    pub source_payload_hash: String,
    pub source_archive: Option<String>,
    pub source_member_path: Option<String>,
    pub hierarchy_path: Vec<String>,
    pub publisher_edges: Vec<CanonicalGraphEdge>,
    pub chunks: Vec<CanonicalChunk>,
    pub canonical_version: String,
}

impl CanonicalDocument {
    pub fn validate(&self) -> Result<(), CanonicalValidationError> {
        if self.source != "legi" {
            return Err(CanonicalValidationError::InvalidSource {
                actual: self.source.clone(),
            });
        }
        if self.kind != "article" {
            return Err(CanonicalValidationError::InvalidKind {
                kind: self.kind.clone(),
            });
        }
        validate_id(
            "source_uid",
            &self.source_uid,
            "LEGIARTI",
            "LEGIARTI[0-9]{12}",
        )
        .map_err(|_| CanonicalValidationError::InvalidSourceUid {
            source_uid: self.source_uid.clone(),
        })?;
        if self.document_id != format!("legi:{}@{}", self.source_uid, self.valid_from) {
            return Err(CanonicalValidationError::InvalidDocumentId {
                document_id: self.document_id.clone(),
            });
        }
        validate_date("valid_from", &self.valid_from).map_err(|_| {
            CanonicalValidationError::InvalidDate {
                field: "valid_from",
                value: self.valid_from.clone(),
            }
        })?;
        if let Some(valid_to) = &self.valid_to {
            validate_date("valid_to", valid_to).map_err(|_| {
                CanonicalValidationError::InvalidDate {
                    field: "valid_to",
                    value: valid_to.clone(),
                }
            })?;
        }
        if self.body.trim().is_empty() {
            return Err(CanonicalValidationError::EmptyBody);
        }
        if !self.source_payload_hash.starts_with("sha256:") {
            return Err(CanonicalValidationError::InvalidPayloadHash {
                source_payload_hash: self.source_payload_hash.clone(),
            });
        }
        for (expected_index, chunk) in self.chunks.iter().enumerate() {
            chunk.validate_for_document(self, expected_index)?;
        }
        Ok(())
    }
}

/// Publisher-provided relationship evidence extracted from a canonical document.
///
/// Phase 0.5 emits these as unresolved candidates: `relation` is conservative
/// (`refers_to`), `edge_source` is `publisher`, and `to_document_id` stays
/// `None` until the graph materialization step resolves `to_source_uid`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalGraphEdge {
    pub edge_id: String,
    pub from_document_id: String,
    pub from_source_uid: String,
    pub to_source_uid: Option<String>,
    pub to_document_id: Option<String>,
    pub relation: String,
    pub edge_source: String,
    pub source_tag: String,
    pub source_text: Option<String>,
    pub source_payload_hash: String,
    pub source_archive: Option<String>,
    pub source_member_path: Option<String>,
    pub attributes: Vec<GraphEdgeAttribute>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdgeAttribute {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalChunk {
    pub chunk_id: String,
    pub document_id: String,
    pub chunk_index: usize,
    pub body: String,
    pub contextualized_body: String,
    pub chunk_kind: String,
    pub chunking: String,
    pub boundary: String,
    pub source_fields: Vec<String>,
    pub source_payload_hash: String,
    pub chunk_builder_version: String,
    pub hierarchy_path: Vec<String>,
}

#[derive(Debug, Error)]
pub enum CanonicalValidationError {
    #[error("canonical document source must be `legi`, got `{actual}`")]
    InvalidSource { actual: String },
    #[error("canonical document kind must be `article`, got `{kind}`")]
    InvalidKind { kind: String },
    #[error("canonical document source_uid is not a LEGI article id: `{source_uid}`")]
    InvalidSourceUid { source_uid: String },
    #[error("canonical document_id does not match legi:<source_uid>@<valid_from>: `{document_id}`")]
    InvalidDocumentId { document_id: String },
    #[error("canonical document has invalid {field}: `{value}`")]
    InvalidDate { field: &'static str, value: String },
    #[error("canonical document body must not be empty")]
    EmptyBody,
    #[error(
        "canonical document source_payload_hash must be sha256-prefixed: `{source_payload_hash}`"
    )]
    InvalidPayloadHash { source_payload_hash: String },
    #[error("canonical chunk `{chunk_id}` is invalid: {message}")]
    InvalidChunk { chunk_id: String, message: String },
}

impl CanonicalChunk {
    fn validate_for_document(
        &self,
        document: &CanonicalDocument,
        expected_index: usize,
    ) -> Result<(), CanonicalValidationError> {
        let expected_chunk_id = format!("chunk:{}:{}", document.document_id, self.chunk_index);
        if self.document_id != document.document_id {
            return Err(invalid_chunk(
                self,
                "document_id does not match parent document",
            ));
        }
        if self.chunk_index != expected_index {
            return Err(invalid_chunk(
                self,
                format!("chunk_index must be {expected_index}"),
            ));
        }
        if self.chunk_id != expected_chunk_id {
            return Err(invalid_chunk(
                self,
                format!("chunk_id must be `{expected_chunk_id}`"),
            ));
        }
        if self.body.trim().is_empty() {
            return Err(invalid_chunk(self, "body must not be empty"));
        }
        if !self.source_payload_hash.starts_with("sha256:") {
            return Err(invalid_chunk(
                self,
                "source_payload_hash must be sha256-prefixed",
            ));
        }
        if self.chunking != "structural" {
            return Err(invalid_chunk(self, "chunking must be `structural`"));
        }
        Ok(())
    }
}

fn invalid_chunk(chunk: &CanonicalChunk, message: impl Into<String>) -> CanonicalValidationError {
    CanonicalValidationError::InvalidChunk {
        chunk_id: chunk.chunk_id.clone(),
        message: message.into(),
    }
}

#[derive(Debug, Error)]
pub enum LegiParseError {
    #[error("xml parse error: {message}")]
    Xml { message: String },
    #[error("missing required field `{field}` for LEGI {entity}")]
    MissingRequiredField {
        entity: &'static str,
        field: &'static str,
    },
    #[error("invalid date in `{field}`: `{value}`")]
    InvalidDate { field: &'static str, value: String },
    #[error("invalid id in `{field}`: `{value}`; expected {expected}")]
    InvalidId {
        field: &'static str,
        value: String,
        expected: &'static str,
    },
}

#[derive(Debug, Default)]
struct RawArticle {
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
    publisher_links: Vec<RawPublisherLink>,
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
struct RawTextStruct {
    id: Option<String>,
    url: Option<String>,
    nature: Option<String>,
    cid: Option<String>,
    num: Option<String>,
    nor: Option<String>,
    date_publi: Option<String>,
    date_texte: Option<String>,
    source_date_debut_hint: Option<String>,
    structure_links: Vec<ParsedTextStructLink>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawPublisherLink {
    source_tag: String,
    text: String,
    attributes: Vec<GraphEdgeAttribute>,
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

fn build_article_chunks(document: &CanonicalDocument) -> Vec<CanonicalChunk> {
    let context = article_chunk_context(document);
    let contextualized_body = contextualized_article_body(&context, &document.body);
    if contextualized_body.chars().count() <= LEGI_ARTICLE_CONTEXTUALIZED_CHUNK_MAX_CHARS {
        return vec![build_article_chunk(
            document,
            &context,
            0,
            document.body.clone(),
            "article",
            vec!["BLOC_TEXTUEL/CONTENU".to_owned()],
        )];
    }

    let units = structural_article_body_units(&document.body);
    if units.len() <= 1 {
        return vec![build_article_chunk(
            document,
            &context,
            0,
            document.body.clone(),
            "article",
            vec!["BLOC_TEXTUEL/CONTENU".to_owned()],
        )];
    }

    let mut chunks = Vec::new();
    let mut current_units = Vec::new();
    let mut current_start = 1usize;

    for (index, unit) in units.iter().enumerate() {
        let candidate = join_article_body_units(&current_units, Some(unit));
        if !current_units.is_empty()
            && contextualized_article_body(&context, &candidate)
                .chars()
                .count()
                > LEGI_ARTICLE_CONTEXTUALIZED_CHUNK_MAX_CHARS
        {
            push_alinea_chunk(
                document,
                &context,
                &mut chunks,
                &current_units,
                current_start,
                index,
            );
            current_units.clear();
            current_start = index + 1;
        }
        current_units.push(*unit);
    }

    if !current_units.is_empty() {
        push_alinea_chunk(
            document,
            &context,
            &mut chunks,
            &current_units,
            current_start,
            units.len(),
        );
    }

    if chunks.len() <= 1 {
        vec![build_article_chunk(
            document,
            &context,
            0,
            document.body.clone(),
            "article",
            vec!["BLOC_TEXTUEL/CONTENU".to_owned()],
        )]
    } else {
        chunks
    }
}

fn article_chunk_context(document: &CanonicalDocument) -> String {
    let mut parts = document.hierarchy_path.clone();
    if let Some(title) = &document.title {
        parts.push(title.clone());
    }
    parts.join(" > ")
}

fn contextualized_article_body(context: &str, body: &str) -> String {
    if context.is_empty() {
        body.to_owned()
    } else {
        format!("{context}\n\n{body}")
    }
}

fn structural_article_body_units(body: &str) -> Vec<&str> {
    // ARTICLE body assembly already collapses inline whitespace and emits one
    // '\n' per block boundary; split chunks can trim/drop empty lines and
    // rejoin units without changing the canonical text for current LEGI input.
    body.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect()
}

fn join_article_body_units(units: &[&str], extra: Option<&str>) -> String {
    let mut body = units.join("\n");
    if let Some(extra) = extra {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(extra);
    }
    body
}

fn push_alinea_chunk(
    document: &CanonicalDocument,
    context: &str,
    chunks: &mut Vec<CanonicalChunk>,
    units: &[&str],
    start: usize,
    end: usize,
) {
    let boundary = if start == end {
        "alinea"
    } else {
        "alinea_range"
    };
    let source_fields = vec![
        "BLOC_TEXTUEL/CONTENU".to_owned(),
        format!("BLOC_TEXTUEL/CONTENU/alinea:{start}-{end}"),
    ];
    chunks.push(build_article_chunk(
        document,
        context,
        chunks.len(),
        join_article_body_units(units, None),
        boundary,
        source_fields,
    ));
}

fn build_article_chunk(
    document: &CanonicalDocument,
    context: &str,
    chunk_index: usize,
    body: String,
    boundary: &str,
    source_fields: Vec<String>,
) -> CanonicalChunk {
    CanonicalChunk {
        chunk_id: format!("chunk:{}:{chunk_index}", document.document_id),
        document_id: document.document_id.clone(),
        chunk_index,
        contextualized_body: contextualized_article_body(context, &body),
        body,
        chunk_kind: "article_body".to_owned(),
        chunking: "structural".to_owned(),
        boundary: boundary.to_owned(),
        source_fields,
        source_payload_hash: document.source_payload_hash.clone(),
        chunk_builder_version: LEGI_ARTICLE_CHUNK_BUILDER_VERSION.to_owned(),
        hierarchy_path: document.hierarchy_path.clone(),
    }
}

fn required(
    entity: &'static str,
    field: &'static str,
    value: Option<String>,
) -> Result<String, LegiParseError> {
    let value = value.ok_or(LegiParseError::MissingRequiredField { entity, field })?;
    required_non_empty(entity, field, value)
}

fn required_non_empty(
    entity: &'static str,
    field: &'static str,
    value: String,
) -> Result<String, LegiParseError> {
    if value.trim().is_empty() {
        Err(LegiParseError::MissingRequiredField { entity, field })
    } else {
        Ok(value.trim().to_owned())
    }
}

fn optional_non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
}

impl RawPublisherLink {
    fn into_edge(self, index: usize, document: &CanonicalDocument) -> CanonicalGraphEdge {
        let to_source_uid = self.target_source_uid();
        let source_text = optional_non_empty(Some(self.text));
        let edge_id = publisher_edge_id(
            document.document_id.as_str(),
            index,
            self.source_tag.as_str(),
            to_source_uid.as_deref(),
            source_text.as_deref(),
        );

        CanonicalGraphEdge {
            edge_id,
            from_document_id: document.document_id.clone(),
            from_source_uid: document.source_uid.clone(),
            to_source_uid,
            to_document_id: None,
            relation: "refers_to".to_owned(),
            edge_source: "publisher".to_owned(),
            source_tag: self.source_tag,
            source_text,
            source_payload_hash: document.source_payload_hash.clone(),
            source_archive: document.source_archive.clone(),
            source_member_path: document.source_member_path.clone(),
            attributes: self.attributes,
        }
    }

    fn target_source_uid(&self) -> Option<String> {
        ["id", "cid", "cidtexte", "href"]
            .iter()
            .find_map(|key| self.attribute_value(key))
            .map(|value| extract_known_source_uid(value.as_str()).unwrap_or(value))
    }

    fn attribute_value(&self, key: &str) -> Option<String> {
        self.attributes
            .iter()
            .find(|attribute| attribute.key == key)
            .and_then(|attribute| optional_non_empty(Some(attribute.value.clone())))
    }
}

fn push_publisher_link(
    raw: &mut RawArticle,
    start: &BytesStart<'_>,
    source_tag: &str,
) -> Result<usize, LegiParseError> {
    let attributes = collect_attributes(start)?;
    raw.publisher_links.push(RawPublisherLink {
        source_tag: source_tag.to_owned(),
        text: String::new(),
        attributes,
    });
    Ok(raw.publisher_links.len() - 1)
}

fn collect_attributes(start: &BytesStart<'_>) -> Result<Vec<GraphEdgeAttribute>, LegiParseError> {
    let mut attributes = Vec::new();
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| LegiParseError::Xml {
            message: error.to_string(),
        })?;
        let value = attribute
            .decode_and_unescape_value(start.decoder())
            .map_err(|error| LegiParseError::Xml {
                message: error.to_string(),
            })?;
        attributes.push(GraphEdgeAttribute {
            key: attribute_name(attribute.key.as_ref()),
            value: value.into_owned(),
        });
    }
    Ok(attributes)
}

fn push_text_struct_link(
    raw: &mut RawTextStruct,
    start: &BytesStart<'_>,
    source_tag: &str,
) -> Result<usize, LegiParseError> {
    let attributes = collect_attributes(start)?;
    let target_source_uid = text_struct_link_target_source_uid(&attributes);
    let level =
        text_struct_link_attribute(&attributes, "niv").and_then(|value| value.parse::<i32>().ok());
    let debut = text_struct_link_attribute(&attributes, "debut");
    let fin = text_struct_link_attribute(&attributes, "fin");
    let order = raw.structure_links.len();
    raw.structure_links.push(ParsedTextStructLink {
        source_tag: source_tag.to_owned(),
        order,
        target_source_uid,
        level,
        debut,
        fin,
        text: None,
        attributes,
    });
    Ok(order)
}

fn text_struct_link_attribute(attributes: &[GraphEdgeAttribute], key: &str) -> Option<String> {
    attributes
        .iter()
        .find(|attribute| attribute.key == key)
        .and_then(|attribute| optional_non_empty(Some(attribute.value.clone())))
}

fn text_struct_link_target_source_uid(attributes: &[GraphEdgeAttribute]) -> Option<String> {
    ["id", "cid", "cidtexte", "href"].iter().find_map(|key| {
        text_struct_link_attribute(attributes, key)
            .and_then(|value| extract_known_source_uid(value.as_str()))
    })
}

fn assign_text_struct_link_text(raw: &mut RawTextStruct, link_stack: &[usize], value: &str) {
    let Some(link) = link_stack
        .last()
        .and_then(|index| raw.structure_links.get_mut(*index))
    else {
        return;
    };
    let mut text = link.text.clone().unwrap_or_default();
    append_xml_content(&mut text, value);
    link.text = if text.trim().is_empty() {
        None
    } else {
        Some(text)
    };
}

fn is_text_struct_link_tag(name: &str) -> bool {
    matches!(name, "LIEN_TXT" | "LIEN_SECTION_TA" | "LIEN_ART")
}

fn attribute_value(start: &BytesStart<'_>, wanted: &str) -> Result<Option<String>, LegiParseError> {
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| LegiParseError::Xml {
            message: error.to_string(),
        })?;
        if attribute_name(attribute.key.as_ref()) != wanted {
            continue;
        }
        let value = attribute
            .decode_and_unescape_value(start.decoder())
            .map_err(|error| LegiParseError::Xml {
                message: error.to_string(),
            })?;
        return Ok(Some(value.into_owned()));
    }
    Ok(None)
}

fn assign_link_text(raw: &mut RawArticle, link_stack: &[usize], value: &str) {
    if let Some(link) = link_stack
        .last()
        .and_then(|index| raw.publisher_links.get_mut(*index))
    {
        append_xml_content(&mut link.text, value);
    }
}

fn is_publisher_link_tag(name: &str) -> bool {
    matches!(
        name,
        "LIEN" | "LIEN_ART" | "LIEN_SECTION_TA" | "LIEN_TXT" | "a" | "A"
    )
}

fn publisher_edge_id(
    from_document_id: &str,
    index: usize,
    source_tag: &str,
    to_source_uid: Option<&str>,
    source_text: Option<&str>,
) -> String {
    let evidence = format!(
        "{from_document_id}|{index}|{source_tag}|{}|{}",
        to_source_uid.unwrap_or_default(),
        source_text.unwrap_or_default()
    );
    let hash = source_payload_hash(evidence.as_bytes());
    let digest = hash.strip_prefix("sha256:").unwrap_or(hash.as_str());
    format!("publisher-edge:{digest}")
}

fn extract_known_source_uid(value: &str) -> Option<String> {
    ["LEGIARTI", "LEGISCTA", "LEGITEXT", "JORFTEXT"]
        .iter()
        .find_map(|prefix| {
            let start = value.find(prefix)?;
            let suffix = value[start + prefix.len()..]
                .chars()
                .take_while(|character| character.is_ascii_digit())
                .take(12)
                .collect::<String>();
            if suffix.len() == 12 {
                Some(format!("{prefix}{suffix}"))
            } else {
                None
            }
        })
}

fn validate_id(
    field: &'static str,
    value: &str,
    prefix: &'static str,
    expected: &'static str,
) -> Result<(), LegiParseError> {
    let suffix = value
        .strip_prefix(prefix)
        .ok_or(LegiParseError::InvalidId {
            field,
            value: value.to_owned(),
            expected,
        })?;
    if suffix.len() == 12 && suffix.chars().all(|character| character.is_ascii_digit()) {
        Ok(())
    } else {
        Err(LegiParseError::InvalidId {
            field,
            value: value.to_owned(),
            expected,
        })
    }
}

fn normalize_required_date(field: &'static str, value: &str) -> Result<String, LegiParseError> {
    validate_date(field, value)?;
    Ok(value.to_owned())
}

fn normalize_end_date(field: &'static str, value: &str) -> Result<Option<String>, LegiParseError> {
    validate_date(field, value)?;
    if matches!(value, "2999-01-01" | "2999-12-31") {
        Ok(None)
    } else {
        Ok(Some(value.to_owned()))
    }
}

fn validate_date(field: &'static str, value: &str) -> Result<(), LegiParseError> {
    let bytes = value.as_bytes();
    let valid_shape = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit());
    if !valid_shape {
        return Err(LegiParseError::InvalidDate {
            field,
            value: value.to_owned(),
        });
    }
    let year = value[0..4].parse::<u16>().unwrap_or_default();
    let month = value[5..7].parse::<u8>().unwrap_or_default();
    let day = value[8..10].parse::<u8>().unwrap_or_default();
    if day > 0 && day <= days_in_month(year, month).unwrap_or_default() {
        Ok(())
    } else {
        Err(LegiParseError::InvalidDate {
            field,
            value: value.to_owned(),
        })
    }
}

fn days_in_month(year: u16, month: u8) -> Option<u8> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if is_leap_year(year) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

fn is_leap_year(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn assign_if_empty(slot: &mut Option<String>, value: &str) {
    if slot.is_none() {
        *slot = Some(value.to_owned());
    }
}

fn append_xml_content(buffer: &mut String, value: &str) {
    for character in value.chars() {
        if character.is_whitespace() {
            if !buffer.is_empty()
                && !buffer
                    .chars()
                    .last()
                    .is_some_and(|last| last.is_whitespace())
            {
                buffer.push(' ');
            }
        } else {
            buffer.push(character);
        }
    }
}

fn append_block_boundary(buffer: &mut String) {
    let trimmed_len = buffer.trim_end_matches(' ').len();
    buffer.truncate(trimmed_len);
    if !buffer.is_empty() && !buffer.ends_with('\n') {
        buffer.push('\n');
    }
}

fn is_body_block_boundary(name: &str) -> bool {
    matches!(
        name,
        "p" | "P"
            | "br"
            | "BR"
            | "li"
            | "LI"
            | "div"
            | "DIV"
            | "blockquote"
            | "BLOCKQUOTE"
            | "tr"
            | "TR"
            | "td"
            | "TD"
            | "th"
            | "TH"
            | "table"
            | "TABLE"
    )
}

fn resolve_reference(reference: &BytesRef<'_>) -> Result<String, LegiParseError> {
    match reference
        .decode()
        .map_err(|error| LegiParseError::Xml {
            message: error.to_string(),
        })?
        .as_ref()
    {
        "amp" => Ok("&".to_owned()),
        "lt" => Ok("<".to_owned()),
        "gt" => Ok(">".to_owned()),
        "quot" => Ok("\"".to_owned()),
        "apos" => Ok("'".to_owned()),
        _ => match reference
            .resolve_char_ref()
            .map_err(|error| LegiParseError::Xml {
                message: error.to_string(),
            })? {
            Some(character) => Ok(character.to_string()),
            None => Err(LegiParseError::Xml {
                message: format!(
                    "unsupported XML entity reference `{}`",
                    reference.decode().unwrap_or_default()
                ),
            }),
        },
    }
}

fn path_ends_with(stack: &[String], tail: &[&str]) -> bool {
    stack.len() >= tail.len()
        && stack[stack.len() - tail.len()..]
            .iter()
            .map(String::as_str)
            .eq(tail.iter().copied())
}

fn path_contains(stack: &[String], needle: &[&str]) -> bool {
    !needle.is_empty()
        && stack.len() >= needle.len()
        && stack
            .windows(needle.len())
            .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
}

fn local_name(name: &[u8]) -> String {
    String::from_utf8_lossy(name).into_owned()
}

fn attribute_name(name: &[u8]) -> String {
    let name = local_name(name);
    name.rsplit_once(':')
        .map(|(_, local)| local.to_owned())
        .unwrap_or(name)
}

pub fn source_payload_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests;
