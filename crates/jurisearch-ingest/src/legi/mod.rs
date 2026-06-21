use std::fmt::Write as _;

use quick_xml::{
    Reader,
    events::{BytesRef, Event},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::archive::ArchiveMember;

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
            .or_else(|| Some(member.archive_path.display().to_string()));

        Self {
            archive_name,
            member_path: Some(member.member_path.clone()),
            payload_hash: Some(sha256_hex(&member.bytes)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParsedLegiXml {
    Article(Box<CanonicalDocument>),
    UnsupportedRoot { root: String },
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
        Ok(())
    }
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
                return Err(LegiParseError::Xml {
                    message: "missing XML root element".to_owned(),
                });
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
    let mut raw = RawArticle::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => stack.push(local_name(start.local_name().as_ref())),
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
                assign_article_text(&mut raw, &stack, value.as_ref());
            }
            Ok(Event::CData(text)) => {
                let value = String::from_utf8_lossy(text.as_ref());
                assign_article_text(&mut raw, &stack, value.as_ref());
            }
            Ok(Event::GeneralRef(reference)) => {
                let value = resolve_reference(&reference)?;
                assign_article_text(&mut raw, &stack, value.as_str());
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
        let num = required("article", "META_ARTICLE/NUM", self.num)?;
        let article_type = required("article", "META_ARTICLE/TYPE", self.article_type)?;
        let valid_from = normalize_required_date(
            "META_ARTICLE/DATE_DEBUT",
            &required("article", "META_ARTICLE/DATE_DEBUT", self.date_debut)?,
        )?;
        let valid_to_raw = required("article", "META_ARTICLE/DATE_FIN", self.date_fin)?;
        let valid_to = normalize_end_date("META_ARTICLE/DATE_FIN", &valid_to_raw)?;
        let body = required_non_empty("article", "BLOC_TEXTUEL/CONTENU", self.body)?;
        let source_payload_hash = provenance
            .payload_hash
            .unwrap_or_else(|| sha256_hex(xml.as_bytes()));
        let title = format!("Article {num}");
        let citation_prefix = self
            .hierarchy_path
            .first()
            .cloned()
            .unwrap_or_else(|| "LEGI".to_owned());

        let document = CanonicalDocument {
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
            source_article_type: Some(article_type.clone()),
            valid_from,
            valid_to,
            valid_to_raw: Some(valid_to_raw),
            source_url: self.url,
            source_payload_hash,
            source_archive: provenance.archive_name,
            source_member_path: provenance.member_path,
            hierarchy_path: self.hierarchy_path,
            canonical_version: format!(
                "legi_article:v1:nature={nature}:etat={}:type={article_type}",
                etat.as_deref().unwrap_or("absent")
            ),
        };
        document.validate().map_err(|error| LegiParseError::Xml {
            message: format!("canonical validation failed: {error}"),
        })?;
        Ok(document)
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

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::archive::ArchiveMember;

    use super::{
        CanonicalDocument, LegiParseError, ParsedLegiXml, SourceProvenance, parse_legi_member,
        parse_legi_xml, sha256_hex,
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
        };

        assert_eq!(
            document.source_archive.as_deref(),
            Some("Freemium_legi_global.tar.gz")
        );
        assert_eq!(
            document.source_member_path.as_deref(),
            Some("legi/articles/LEGIARTI000006419320.xml")
        );
        assert_eq!(document.source_payload_hash, sha256_hex(&member.bytes));
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
        let error = parse_article_fixture(&article_fixture().replace("1804-02-21", "1804-99-21"))
            .unwrap_err();

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
        let error =
            parse_article_fixture(&article_fixture().replace("LEGIARTI000006419320", "BAD"))
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
            "<TEXTELR><META><META_COMMUN><ID>LEGITEXT000006070721</ID></META_COMMUN></META></TEXTELR>",
            provenance(),
        )
        .unwrap();

        assert_eq!(
            parsed,
            ParsedLegiXml::UnsupportedRoot {
                root: "TEXTELR".to_owned()
            }
        );
    }

    fn parse_article_fixture(xml: &str) -> Result<CanonicalDocument, LegiParseError> {
        match parse_legi_xml(xml, provenance())? {
            ParsedLegiXml::Article(document) => Ok(*document),
            ParsedLegiXml::UnsupportedRoot { root } => {
                panic!("expected article, got unsupported root {root}")
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
</ARTICLE>
"#
        .to_owned()
    }
}
