//! DILA bulk jurisprudence ingestion: `TEXTE_JURI_JUDI` (judicial: CASS/CAPP/INCA) and
//! `TEXTE_JURI_ADMIN` (administrative: JADE) official XML → canonical decision records.
//!
//! Per the Phase 2 scope decision (`work/03-implementation/02-evidence/
//! 2026-06-23-phase2-jurisprudence-ingestion-scope-decision.md`), DILA bulk is the primary
//! offline full-corpus jurisprudence path. Bulk XML carries no official Judilibre zone offsets, so
//! chunking is honestly flagged `heuristic` and never satisfies the official-zone gate by assertion.
//! Decisions are *dated, not versioned*: `decision_date` is canonical and `valid_to` is always null.

use std::collections::BTreeMap;

use quick_xml::{
    Reader,
    events::{BytesRef, BytesStart, Event},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::archive::{ArchiveMember, ArchiveSource};
use crate::legi::{
    CanonicalChunk, CanonicalGraphEdge, GraphEdgeAttribute, SourceProvenance, source_payload_hash,
};

const JURI_DECISION_CANONICAL_VERSION: &str = "juri_decision:v1";
const JURI_DECISION_CHUNK_BUILDER_VERSION: &str = "juri_decision_heuristic:v1";
/// Conservative per-chunk character budget for heuristic body splitting. Mirrors the LEGI article
/// contextualized-chunk ceiling so decision chunks stay inside the embedding endpoint budget.
const JURI_DECISION_CHUNK_MAX_CHARS: usize = 6_000;
const JURI_EMPTY_XML_ROOT: &str = "EMPTY_XML";

const ROOT_JUDI: &str = "TEXTE_JURI_JUDI";
const ROOT_ADMIN: &str = "TEXTE_JURI_ADMIN";

/// Result of parsing one bulk jurisprudence XML member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParsedJuriXml {
    Decision(Box<CanonicalDecision>),
    /// A root we do not (yet) project into canonical decisions. Counted, never silently inserted.
    UnsupportedRoot { root: String },
}

impl ParsedJuriXml {
    #[must_use]
    pub fn root_name(&self) -> &str {
        match self {
            Self::Decision(decision) => decision.source_family.root_element(),
            Self::UnsupportedRoot { root } => root.as_str(),
        }
    }

    #[must_use]
    pub fn source_uid(&self) -> Option<&str> {
        match self {
            Self::Decision(decision) => Some(decision.source_uid.as_str()),
            Self::UnsupportedRoot { .. } => None,
        }
    }
}

/// Whether a decision came from the judicial or administrative DILA jurisprudence family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JuriFamily {
    /// `TEXTE_JURI_JUDI` — Cour de cassation / cours d'appel (CASS/CAPP/INCA).
    Judicial,
    /// `TEXTE_JURI_ADMIN` — Conseil d'État / CAA / TA (JADE).
    Administrative,
}

impl JuriFamily {
    #[must_use]
    pub fn root_element(self) -> &'static str {
        match self {
            JuriFamily::Judicial => ROOT_JUDI,
            JuriFamily::Administrative => ROOT_ADMIN,
        }
    }

    /// Coverage tag used by status reporting, e.g. `dila_juri_judi` / `dila_juri_admin`.
    #[must_use]
    pub fn coverage_tag(self) -> &'static str {
        match self {
            JuriFamily::Judicial => "dila_juri_judi",
            JuriFamily::Administrative => "dila_juri_admin",
        }
    }

    /// The expected source-native UID prefix for this family.
    #[must_use]
    pub fn uid_prefix(self) -> &'static str {
        match self {
            JuriFamily::Judicial => "JURITEXT",
            JuriFamily::Administrative => "CETATEXT",
        }
    }
}

/// One `SOMMAIRE` titrage/résumé pair (`SCT` heading + matching `ANA` abstract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionSummary {
    pub id: Option<String>,
    /// `PRINCIPAL` or `REFERENCE` for titrage headings; `analyse` for `ANA` abstracts.
    pub kind: String,
    pub text: String,
}

/// Canonical decision record produced from official DILA bulk jurisprudence XML.
///
/// Shared decision core plus family-specific raw metadata preserved in `raw_metadata` so later
/// corrections / Judilibre enrichment never need to re-parse the archive. Source-native IDs stay
/// authoritative; `document_id` never pretends a bulk record came from Judilibre.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalDecision {
    pub document_id: String,
    /// Dataset discriminator: `cass` | `capp` | `inca` | `jade`.
    pub source: String,
    pub source_family: JuriFamily,
    pub kind: String,
    /// Source-native UID (`JURITEXT…` / `CETATEXT…`).
    pub source_uid: String,
    pub citation: Option<String>,
    pub title: Option<String>,
    pub body: String,
    /// `DATE_DEC` (ISO `YYYY-MM-DD`). Decisions are dated, not versioned.
    pub decision_date: String,
    pub jurisdiction: Option<String>,
    pub ecli: Option<String>,
    /// `META_JURI/NUMERO` (decision number, e.g. `P2500683` / `24PA03561`).
    pub number: Option<String>,
    pub solution: Option<String>,
    pub formation: Option<String>,
    /// `META_COMMUN/NATURE` (e.g. `ARRET`, `Texte`).
    pub nature: Option<String>,
    /// Publication level: judicial `PUBLI_BULL@publie`, administrative `PUBLI_RECUEIL`.
    pub publication: Option<String>,
    /// `NUMERO_AFFAIRE*` (pourvoi numbers) for judicial decisions.
    pub case_numbers: Vec<String>,
    pub source_url: Option<String>,
    pub source_payload_hash: String,
    pub source_archive: Option<String>,
    pub source_member_path: Option<String>,
    /// Honest chunking provenance for bulk records: always `heuristic` (no official zones).
    pub chunking_provenance: String,
    /// All raw XML-derived scalar metadata, preserved verbatim by element name.
    pub raw_metadata: BTreeMap<String, String>,
    pub summaries: Vec<DecisionSummary>,
    pub publisher_edges: Vec<CanonicalGraphEdge>,
    pub chunks: Vec<CanonicalChunk>,
    pub canonical_version: String,
}

impl CanonicalDecision {
    /// Validate canonical invariants before storage projection.
    pub fn validate(&self) -> Result<(), DecisionValidationError> {
        if self.kind != "decision" {
            return Err(DecisionValidationError::InvalidKind {
                kind: self.kind.clone(),
            });
        }
        let is_jurisprudence_source = ArchiveSource::from_token(&self.source)
            .map(ArchiveSource::is_jurisprudence)
            .unwrap_or(false);
        if !is_jurisprudence_source {
            return Err(DecisionValidationError::InvalidSource {
                dataset: self.source.clone(),
            });
        }
        if !self.source_uid.starts_with(self.source_family.uid_prefix()) {
            return Err(DecisionValidationError::InvalidSourceUid {
                source_uid: self.source_uid.clone(),
                expected_prefix: self.source_family.uid_prefix(),
            });
        }
        if self.document_id != format!("{}:{}", self.source, self.source_uid) {
            return Err(DecisionValidationError::InvalidDocumentId {
                document_id: self.document_id.clone(),
            });
        }
        validate_iso_date(&self.decision_date).map_err(|_| {
            DecisionValidationError::InvalidDecisionDate {
                value: self.decision_date.clone(),
            }
        })?;
        if self.body.trim().is_empty() {
            return Err(DecisionValidationError::EmptyBody);
        }
        if !self.source_payload_hash.starts_with("sha256:") {
            return Err(DecisionValidationError::InvalidPayloadHash {
                source_payload_hash: self.source_payload_hash.clone(),
            });
        }
        for (expected_index, chunk) in self.chunks.iter().enumerate() {
            self.validate_chunk(chunk, expected_index)?;
        }
        Ok(())
    }

    fn validate_chunk(
        &self,
        chunk: &CanonicalChunk,
        expected_index: usize,
    ) -> Result<(), DecisionValidationError> {
        let expected_chunk_id = format!("chunk:{}:{}", self.document_id, chunk.chunk_index);
        if chunk.document_id != self.document_id {
            return Err(DecisionValidationError::InvalidChunk {
                chunk_id: chunk.chunk_id.clone(),
                message: "document_id does not match parent decision".to_owned(),
            });
        }
        if chunk.chunk_index != expected_index {
            return Err(DecisionValidationError::InvalidChunk {
                chunk_id: chunk.chunk_id.clone(),
                message: format!("chunk_index must be {expected_index}"),
            });
        }
        if chunk.chunk_id != expected_chunk_id {
            return Err(DecisionValidationError::InvalidChunk {
                chunk_id: chunk.chunk_id.clone(),
                message: format!("chunk_id must be `{expected_chunk_id}`"),
            });
        }
        if chunk.body.trim().is_empty() {
            return Err(DecisionValidationError::InvalidChunk {
                chunk_id: chunk.chunk_id.clone(),
                message: "body must not be empty".to_owned(),
            });
        }
        if chunk.chunking != "heuristic" {
            return Err(DecisionValidationError::InvalidChunk {
                chunk_id: chunk.chunk_id.clone(),
                message: "bulk decision chunking must be `heuristic`".to_owned(),
            });
        }
        if !chunk.source_payload_hash.starts_with("sha256:") {
            return Err(DecisionValidationError::InvalidChunk {
                chunk_id: chunk.chunk_id.clone(),
                message: "source_payload_hash must be sha256-prefixed".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum DecisionValidationError {
    #[error("canonical decision kind must be `decision`, got `{kind}`")]
    InvalidKind { kind: String },
    #[error("canonical decision source must be a jurisprudence dataset, got `{dataset}`")]
    InvalidSource { dataset: String },
    #[error("canonical decision source_uid `{source_uid}` must start with `{expected_prefix}`")]
    InvalidSourceUid {
        source_uid: String,
        expected_prefix: &'static str,
    },
    #[error("canonical decision document_id does not match <source>:<source_uid>: `{document_id}`")]
    InvalidDocumentId { document_id: String },
    #[error("canonical decision decision_date is invalid: `{value}`")]
    InvalidDecisionDate { value: String },
    #[error("canonical decision body must not be empty")]
    EmptyBody,
    #[error(
        "canonical decision source_payload_hash must be sha256-prefixed: `{source_payload_hash}`"
    )]
    InvalidPayloadHash { source_payload_hash: String },
    #[error("canonical decision chunk `{chunk_id}` is invalid: {message}")]
    InvalidChunk { chunk_id: String, message: String },
}

#[derive(Debug, Error)]
pub enum JuriParseError {
    #[error("xml parse error: {message}")]
    Xml { message: String },
    #[error("missing required field `{field}` for {entity}")]
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
    #[error("archive member `{member}` is not valid UTF-8 XML: {message}")]
    NotUtf8 { member: String, message: String },
    #[error("unknown jurisprudence source `{dataset}`")]
    UnknownSource { dataset: String },
}

/// Parse a bulk jurisprudence archive member into a canonical decision (or an unsupported-root
/// classification). `source` is the dataset the archive belongs to (`cass`/`capp`/`inca`/`jade`).
pub fn parse_juri_member(
    source: ArchiveSource,
    member: &ArchiveMember,
) -> Result<ParsedJuriXml, JuriParseError> {
    let xml = std::str::from_utf8(&member.bytes).map_err(|error| JuriParseError::NotUtf8 {
        member: member.member_path.clone(),
        message: error.to_string(),
    })?;
    parse_juri_xml(source, xml, SourceProvenance::from_archive_member(member))
}

/// Parse bulk jurisprudence XML into a canonical decision (or an unsupported-root classification).
pub fn parse_juri_xml(
    source: ArchiveSource,
    xml: &str,
    provenance: SourceProvenance,
) -> Result<ParsedJuriXml, JuriParseError> {
    if !source.is_jurisprudence() {
        return Err(JuriParseError::UnknownSource {
            dataset: source.as_str().to_owned(),
        });
    }
    let root = detect_root(xml)?;
    let family = match root.as_str() {
        ROOT_JUDI => JuriFamily::Judicial,
        ROOT_ADMIN => JuriFamily::Administrative,
        _ => return Ok(ParsedJuriXml::UnsupportedRoot { root }),
    };
    let decision = parse_decision(source, family, xml, provenance)?;
    Ok(ParsedJuriXml::Decision(Box::new(decision)))
}

fn detect_root(xml: &str) -> Result<String, JuriParseError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) | Ok(Event::Empty(start)) => {
                return Ok(local_name(start.name().as_ref()));
            }
            Ok(Event::Eof) => return Ok(JURI_EMPTY_XML_ROOT.to_owned()),
            Ok(_) => {}
            Err(error) => {
                return Err(JuriParseError::Xml {
                    message: error.to_string(),
                });
            }
        }
    }
}

#[derive(Default)]
struct RawDecision {
    fields: BTreeMap<String, String>,
    case_numbers: Vec<String>,
    body: BodyAccumulator,
    summaries: Vec<DecisionSummary>,
    current_summary: Option<DecisionSummary>,
    links: Vec<RawLink>,
}

/// Capture the judicial `PUBLI_BULL@publie` flag (`oui`/`non`) under a distinct metadata key so it
/// never collides with any `PUBLI_BULL` text content.
fn capture_publi_bull(raw: &mut RawDecision, start: &BytesStart<'_>) {
    if let Some(publie) = attribute_value(start, "publie") {
        raw.fields.insert("PUBLI_BULL_publie".to_owned(), publie);
    }
}

struct RawLink {
    text: String,
    attributes: Vec<GraphEdgeAttribute>,
}

fn parse_decision(
    source: ArchiveSource,
    family: JuriFamily,
    xml: &str,
    provenance: SourceProvenance,
) -> Result<CanonicalDecision, JuriParseError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::<String>::new();
    let mut raw = RawDecision::default();

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                let name = local_name(start.name().as_ref());
                match name.as_str() {
                    "LIEN" => raw.links.push(RawLink {
                        text: String::new(),
                        attributes: collect_attributes(&start),
                    }),
                    "SCT" => {
                        raw.current_summary = Some(DecisionSummary {
                            id: attribute_value(&start, "ID"),
                            kind: attribute_value(&start, "TYPE")
                                .unwrap_or_else(|| "PRINCIPAL".to_owned()),
                            text: String::new(),
                        });
                    }
                    "ANA" => {
                        raw.current_summary = Some(DecisionSummary {
                            id: attribute_value(&start, "ID"),
                            kind: "analyse".to_owned(),
                            text: String::new(),
                        });
                    }
                    // `<PUBLI_BULL publie="oui">…</PUBLI_BULL>` — capture the publication flag.
                    "PUBLI_BULL" => capture_publi_bull(&mut raw, &start),
                    _ => {}
                }
                stack.push(name);
            }
            Ok(Event::Empty(start)) => {
                let name = local_name(start.name().as_ref());
                match name.as_str() {
                    "LIEN" => raw.links.push(RawLink {
                        text: String::new(),
                        attributes: collect_attributes(&start),
                    }),
                    // The common shape is the self-closing `<PUBLI_BULL publie="oui"/>`.
                    "PUBLI_BULL" => capture_publi_bull(&mut raw, &start),
                    "br" | "BR" => raw.body.push_break(&stack),
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                if let Some(name) = stack.last()
                    && matches!(name.as_str(), "SCT" | "ANA")
                    && let Some(summary) = raw.current_summary.take()
                {
                    let trimmed = collapse_ws(&summary.text);
                    if !trimmed.is_empty() {
                        raw.summaries.push(DecisionSummary {
                            text: trimmed,
                            ..summary
                        });
                    }
                }
                stack.pop();
            }
            Ok(Event::Text(text)) => {
                let value = text.decode().map_err(|error| JuriParseError::Xml {
                    message: error.to_string(),
                })?;
                assign_text(&mut raw, &stack, value.as_ref());
            }
            Ok(Event::CData(text)) => {
                let value = String::from_utf8_lossy(text.as_ref());
                assign_text(&mut raw, &stack, value.as_ref());
            }
            Ok(Event::GeneralRef(reference)) => {
                let value = resolve_reference(&reference)?;
                assign_text(&mut raw, &stack, value.as_str());
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => {
                return Err(JuriParseError::Xml {
                    message: error.to_string(),
                });
            }
        }
    }

    raw.into_decision(source, family, xml, provenance)
}

/// Tags whose text content we capture as scalar metadata (last-write-wins per tag).
fn is_scalar_metadata_tag(name: &str) -> bool {
    matches!(
        name,
        "ID" | "ANCIEN_ID"
            | "ORIGINE"
            | "URL"
            | "NATURE"
            | "TITRE"
            | "DATE_DEC"
            | "JURIDICTION"
            | "NUMERO"
            | "SOLUTION"
            | "ECLI"
            | "FORMATION"
            | "FORM_DEC_ATT"
            | "DATE_DEC_ATT"
            | "SIEGE_APPEL"
            | "JURI_PREM"
            | "LIEU_PREM"
            | "PRESIDENT"
            | "AVOCAT_GL"
            | "AVOCATS"
            | "RAPPORTEUR"
            | "COMMISSAIRE_GVT"
            | "TYPE_REC"
            | "PUBLI_RECUEIL"
            | "PUBLI_BULL"
    )
}

fn assign_text(raw: &mut RawDecision, stack: &[String], value: &str) {
    let Some(current) = stack.last() else {
        return;
    };
    match current.as_str() {
        "NUMERO_AFFAIRE" => {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                raw.case_numbers.push(trimmed.to_owned());
            }
        }
        "SCT" | "ANA" => {
            if let Some(summary) = raw.current_summary.as_mut() {
                summary.text.push_str(value);
            }
        }
        "LIEN" => {
            if let Some(link) = raw.links.last_mut() {
                link.text.push_str(value);
            }
        }
        name if is_scalar_metadata_tag(name) => {
            // Only inside META blocks (TITRE etc. are unique); ignore stray text elsewhere.
            let entry = raw.fields.entry(name.to_owned()).or_default();
            entry.push_str(value);
        }
        _ => {}
    }
    // CONTENU body text lives under BLOC_TEXTUEL/CONTENU; capture regardless of inline tags.
    if stack.iter().any(|tag| tag == "CONTENU")
        && stack.iter().any(|tag| tag == "BLOC_TEXTUEL")
    {
        raw.body.push_text(value);
    }
}

impl RawDecision {
    fn into_decision(
        mut self,
        source: ArchiveSource,
        family: JuriFamily,
        xml: &str,
        provenance: SourceProvenance,
    ) -> Result<CanonicalDecision, JuriParseError> {
        // Normalize whitespace on captured scalar fields.
        for value in self.fields.values_mut() {
            *value = collapse_ws(value);
        }
        self.fields.retain(|_, value| !value.is_empty());

        let source_uid = required(family.entity_name(), "ID", self.fields.get("ID").cloned())?;
        validate_uid(&source_uid, family)?;

        let decision_date = {
            let value = required(
                family.entity_name(),
                "DATE_DEC",
                self.fields.get("DATE_DEC").cloned(),
            )?;
            validate_date_field("DATE_DEC", &value)?;
            value
        };

        let body = self.body.finish();
        let source_payload_hash = provenance
            .payload_hash
            .clone()
            .unwrap_or_else(|| source_payload_hash(xml.as_bytes()));

        let document_id = format!("{}:{source_uid}", source.as_str());
        let title = self.fields.get("TITRE").cloned();
        let jurisdiction = self.fields.get("JURIDICTION").cloned();
        let ecli = self.fields.get("ECLI").cloned();
        let number = self.fields.get("NUMERO").cloned();
        let solution = self.fields.get("SOLUTION").cloned();
        let formation = self.fields.get("FORMATION").cloned();
        let nature = self.fields.get("NATURE").cloned();
        let publication = match family {
            JuriFamily::Judicial => self.fields.get("PUBLI_BULL_publie").cloned(),
            JuriFamily::Administrative => self.fields.get("PUBLI_RECUEIL").cloned(),
        };
        // The XML `URL` element is an internal DILA filesystem path (provenance only); expose a
        // stable public Légifrance jurisprudence URL derived from the source-native UID instead.
        let source_url = Some(format!(
            "https://www.legifrance.gouv.fr/juri/id/{source_uid}"
        ));

        let mut decision = CanonicalDecision {
            document_id,
            source: source.as_str().to_owned(),
            source_family: family,
            kind: "decision".to_owned(),
            source_uid,
            citation: title.clone(),
            title,
            body,
            decision_date,
            jurisdiction,
            ecli,
            number,
            solution,
            formation,
            nature,
            publication,
            case_numbers: self.case_numbers.clone(),
            source_url,
            source_payload_hash,
            source_archive: provenance.archive_name.clone(),
            source_member_path: provenance.member_path.clone(),
            chunking_provenance: "heuristic".to_owned(),
            raw_metadata: self.fields.clone(),
            summaries: self.summaries.clone(),
            publisher_edges: Vec::new(),
            chunks: Vec::new(),
            canonical_version: JURI_DECISION_CANONICAL_VERSION.to_owned(),
        };

        decision.publisher_edges = build_publisher_edges(&decision, &self.links);
        decision.chunks = build_decision_chunks(&decision);
        Ok(decision)
    }
}

impl JuriFamily {
    fn entity_name(self) -> &'static str {
        match self {
            JuriFamily::Judicial => "TEXTE_JURI_JUDI",
            JuriFamily::Administrative => "TEXTE_JURI_ADMIN",
        }
    }
}

/// Build the decision context line prepended to every chunk's contextualized body.
fn decision_context(decision: &CanonicalDecision) -> String {
    let mut parts = Vec::new();
    if let Some(title) = &decision.title {
        parts.push(title.clone());
    } else {
        if let Some(jurisdiction) = &decision.jurisdiction {
            parts.push(jurisdiction.clone());
        }
        parts.push(decision.decision_date.clone());
    }
    if let Some(ecli) = &decision.ecli {
        parts.push(ecli.clone());
    }
    parts.join(" — ")
}

/// Heuristic decision chunking: an optional summary chunk (SOMMAIRE titrage + analyses) followed by
/// the full text split on paragraph boundaries into size-bounded chunks. Always `heuristic`.
fn build_decision_chunks(decision: &CanonicalDecision) -> Vec<CanonicalChunk> {
    let context = decision_context(decision);
    let mut chunks = Vec::new();

    let summary_body = decision
        .summaries
        .iter()
        .map(|summary| summary.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !summary_body.trim().is_empty() {
        chunks.push(make_chunk(
            decision,
            &context,
            chunks.len(),
            summary_body,
            "decision_summary",
            "sommaire",
            vec!["TEXTE/SOMMAIRE".to_owned()],
        ));
    }

    for body in split_body(&decision.body, JURI_DECISION_CHUNK_MAX_CHARS) {
        let boundary = "paragraph";
        chunks.push(make_chunk(
            decision,
            &context,
            chunks.len(),
            body,
            "decision_body",
            boundary,
            vec!["TEXTE/BLOC_TEXTUEL/CONTENU".to_owned()],
        ));
    }

    // A decision with empty body would have failed validation; guarantee at least one chunk only
    // when there is real text. (Validation enforces non-empty body separately.)
    chunks
}

fn make_chunk(
    decision: &CanonicalDecision,
    context: &str,
    chunk_index: usize,
    body: String,
    chunk_kind: &str,
    boundary: &str,
    source_fields: Vec<String>,
) -> CanonicalChunk {
    let contextualized_body = if context.is_empty() {
        body.clone()
    } else {
        format!("{context}\n\n{body}")
    };
    CanonicalChunk {
        chunk_id: format!("chunk:{}:{chunk_index}", decision.document_id),
        document_id: decision.document_id.clone(),
        chunk_index,
        contextualized_body,
        body,
        chunk_kind: chunk_kind.to_owned(),
        chunking: "heuristic".to_owned(),
        boundary: boundary.to_owned(),
        source_fields,
        source_payload_hash: decision.source_payload_hash.clone(),
        chunk_builder_version: JURI_DECISION_CHUNK_BUILDER_VERSION.to_owned(),
        hierarchy_path: Vec::new(),
    }
}

/// Split body text on paragraph boundaries, packing paragraphs into chunks under `max_chars`.
/// A single over-long paragraph is hard-split on character count as a last resort.
fn split_body(body: &str, max_chars: usize) -> Vec<String> {
    let paragraphs: Vec<&str> = body
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if paragraphs.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    for paragraph in paragraphs {
        if paragraph.chars().count() > max_chars {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            for piece in hard_split(paragraph, max_chars) {
                chunks.push(piece);
            }
            continue;
        }
        let projected = if current.is_empty() {
            paragraph.chars().count()
        } else {
            current.chars().count() + 1 + paragraph.chars().count()
        };
        if projected > max_chars && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(paragraph);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn hard_split(text: &str, max_chars: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(max_chars.max(1))
        .map(|chunk| chunk.iter().collect::<String>())
        .collect()
}

/// Build publisher graph edges from `LIENS/LIEN` applied-text references. Bulk/official links are
/// `edge_source = publisher`; targets are resolved when an `id`/`cidtexte` is present, otherwise the
/// raw evidence text is preserved for later resolution.
fn build_publisher_edges(
    decision: &CanonicalDecision,
    links: &[RawLink],
) -> Vec<CanonicalGraphEdge> {
    links
        .iter()
        .enumerate()
        .filter_map(|(index, link)| {
            let text = collapse_ws(&link.text);
            let attributes: Vec<GraphEdgeAttribute> = link
                .attributes
                .iter()
                .filter(|attribute| !attribute.value.trim().is_empty())
                .cloned()
                .collect();
            if text.is_empty() && attributes.is_empty() {
                return None;
            }
            let to_source_uid = link_target_source_uid(&link.attributes);
            let source_text = if text.is_empty() { None } else { Some(text) };
            let edge_id = decision_edge_id(
                &decision.document_id,
                index,
                "LIEN",
                to_source_uid.as_deref(),
                source_text.as_deref(),
            );
            Some(CanonicalGraphEdge {
                edge_id,
                from_document_id: decision.document_id.clone(),
                from_source_uid: decision.source_uid.clone(),
                to_source_uid,
                to_document_id: None,
                relation: "refers_to".to_owned(),
                edge_source: "publisher".to_owned(),
                source_tag: "LIEN".to_owned(),
                source_text,
                source_payload_hash: decision.source_payload_hash.clone(),
                source_archive: decision.source_archive.clone(),
                source_member_path: decision.source_member_path.clone(),
                attributes,
            })
        })
        .collect()
}

fn link_target_source_uid(attributes: &[GraphEdgeAttribute]) -> Option<String> {
    ["id", "cidtexte"].iter().find_map(|key| {
        attributes
            .iter()
            .find(|attribute| attribute.key.eq_ignore_ascii_case(key))
            .map(|attribute| attribute.value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

fn decision_edge_id(
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

// ----- small body accumulator -----------------------------------------------------------------

/// Accumulates CONTENU text, converting `<br/>` (and `<p>` boundaries via blank lines in the source)
/// into paragraph newlines. Whitespace is collapsed at `finish`.
#[derive(Default)]
struct BodyAccumulator {
    buffer: String,
}

impl BodyAccumulator {
    fn push_text(&mut self, value: &str) {
        self.buffer.push_str(value);
    }

    fn push_break(&mut self, stack: &[String]) {
        if stack.iter().any(|tag| tag == "CONTENU") {
            self.buffer.push('\n');
        }
    }

    fn finish(self) -> String {
        self.buffer
            .split('\n')
            .map(collapse_ws_owned)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

// ----- shared small helpers --------------------------------------------------------------------

fn collapse_ws(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collapse_ws_owned(value: &str) -> String {
    collapse_ws(value)
}

fn local_name(name: &[u8]) -> String {
    let name = std::str::from_utf8(name).unwrap_or_default();
    match name.rsplit_once(':') {
        Some((_, local)) => local.to_owned(),
        None => name.to_owned(),
    }
}

/// Resolve a general/character XML entity reference to its text value (predefined entities plus
/// numeric char refs). Mirrors the LEGI parser's handling so decision text decodes identically.
fn resolve_reference(reference: &BytesRef<'_>) -> Result<String, JuriParseError> {
    let name = reference.decode().map_err(|error| JuriParseError::Xml {
        message: error.to_string(),
    })?;
    match name.as_ref() {
        "amp" => Ok("&".to_owned()),
        "lt" => Ok("<".to_owned()),
        "gt" => Ok(">".to_owned()),
        "quot" => Ok("\"".to_owned()),
        "apos" => Ok("'".to_owned()),
        _ => match reference
            .resolve_char_ref()
            .map_err(|error| JuriParseError::Xml {
                message: error.to_string(),
            })? {
            Some(character) => Ok(character.to_string()),
            None => Err(JuriParseError::Xml {
                message: format!(
                    "unsupported XML entity reference `{}`",
                    reference.decode().unwrap_or_default()
                ),
            }),
        },
    }
}

fn collect_attributes(start: &BytesStart<'_>) -> Vec<GraphEdgeAttribute> {
    start
        .attributes()
        .flatten()
        .map(|attribute| GraphEdgeAttribute {
            key: local_name(attribute.key.as_ref()),
            value: attribute
                .unescape_value()
                .map(|value| value.into_owned())
                .unwrap_or_default(),
        })
        .collect()
}

fn attribute_value(start: &BytesStart<'_>, key: &str) -> Option<String> {
    start.attributes().flatten().find_map(|attribute| {
        if local_name(attribute.key.as_ref()).eq_ignore_ascii_case(key) {
            attribute
                .unescape_value()
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        } else {
            None
        }
    })
}

fn required(
    entity: &'static str,
    field: &'static str,
    value: Option<String>,
) -> Result<String, JuriParseError> {
    let value = value
        .map(|value| collapse_ws(&value))
        .filter(|value| !value.is_empty())
        .ok_or(JuriParseError::MissingRequiredField { entity, field })?;
    Ok(value)
}

fn validate_uid(value: &str, family: JuriFamily) -> Result<(), JuriParseError> {
    let prefix = family.uid_prefix();
    let expected: &'static str = match family {
        JuriFamily::Judicial => "JURITEXT[0-9]{12}",
        JuriFamily::Administrative => "CETATEXT[0-9]{12}",
    };
    let suffix = value.strip_prefix(prefix).ok_or(JuriParseError::InvalidId {
        field: "ID",
        value: value.to_owned(),
        expected,
    })?;
    if suffix.len() == 12 && suffix.chars().all(|character| character.is_ascii_digit()) {
        Ok(())
    } else {
        Err(JuriParseError::InvalidId {
            field: "ID",
            value: value.to_owned(),
            expected,
        })
    }
}

fn validate_date_field(field: &'static str, value: &str) -> Result<(), JuriParseError> {
    validate_iso_date(value).map_err(|()| JuriParseError::InvalidDate {
        field,
        value: value.to_owned(),
    })
}

fn validate_iso_date(value: &str) -> Result<(), ()> {
    let bytes = value.as_bytes();
    let valid_shape = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit());
    if !valid_shape {
        return Err(());
    }
    let month = value[5..7].parse::<u8>().unwrap_or_default();
    let day = value[8..10].parse::<u8>().unwrap_or_default();
    if (1..=12).contains(&month) && (1..=31).contains(&day) {
        Ok(())
    } else {
        Err(())
    }
}

#[cfg(test)]
mod tests;
