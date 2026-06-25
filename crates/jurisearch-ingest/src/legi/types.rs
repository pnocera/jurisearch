//! Parsed LEGI domain types (provenance, parsed text/section/struct nodes, parse error).

use super::*;

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
