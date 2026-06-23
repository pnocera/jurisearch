//! DILA bulk jurisprudence ingestion: `TEXTE_JURI_JUDI` (judicial: CASS/CAPP/INCA) and
//! `TEXTE_JURI_ADMIN` (administrative: JADE) official XML → canonical decision records.
//!
//! Per the Phase 2 scope decision (`work/03-implementation/02-evidence/
//! 2026-06-23-phase2-jurisprudence-ingestion-scope-decision.md`), DILA bulk is the primary
//! offline full-corpus jurisprudence path. Bulk XML carries no official Judilibre zone offsets, so
//! chunking is honestly flagged `heuristic` and never satisfies the official-zone gate by assertion.
//! Decisions are *dated, not versioned*: `decision_date` is canonical and `valid_to` is always null.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::LazyLock;

use quick_xml::{
    Reader,
    events::{BytesRef, BytesStart, Event},
};
use regex::Regex;
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

    /// The jurisprudence family each bulk dataset belongs to (`None` for non-jurisprudence sources).
    /// `cass/capp/inca → TEXTE_JURI_JUDI`, `jade → TEXTE_JURI_ADMIN`.
    #[must_use]
    pub fn for_source(source: ArchiveSource) -> Option<JuriFamily> {
        match source {
            ArchiveSource::Cass | ArchiveSource::Capp | ArchiveSource::Inca => {
                Some(JuriFamily::Judicial)
            }
            ArchiveSource::Jade => Some(JuriFamily::Administrative),
            ArchiveSource::Legi => None,
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
    /// Official applied-text links from `LIENS/LIEN` (`edge_source = "publisher"`).
    pub publisher_edges: Vec<CanonicalGraphEdge>,
    /// Lower-trust article references parsed from the decision body text
    /// (`edge_source = "inferred"`). Always distinguishable from publisher edges.
    pub inferred_edges: Vec<CanonicalGraphEdge>,
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
        // The source token must be a jurisprudence dataset AND its family must match the record's
        // declared `source_family` (so a `jade:JURITEXT…` cross-family record cannot pass — WARN 4).
        match ArchiveSource::from_token(&self.source).and_then(JuriFamily::for_source) {
            Some(family) if family == self.source_family => {}
            _ => {
                return Err(DecisionValidationError::InvalidSource {
                    dataset: self.source.clone(),
                });
            }
        }
        // Bulk DILA records are never zone-accurate: enforce honest provenance so a hand-built or
        // mutated record cannot claim zone/structural quality by assertion (WARN 2 / ADR).
        if self.chunking_provenance != "heuristic" {
            return Err(DecisionValidationError::InvalidChunkingProvenance {
                chunking_provenance: self.chunking_provenance.clone(),
            });
        }
        if self.canonical_version != JURI_DECISION_CANONICAL_VERSION {
            return Err(DecisionValidationError::InvalidCanonicalVersion {
                canonical_version: self.canonical_version.clone(),
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
        // Edge-source trust separation: official LIEN edges are `publisher`; body-parsed citations
        // are `inferred`. The two sets must never be conflated.
        for edge in &self.publisher_edges {
            if edge.edge_source != "publisher" {
                return Err(DecisionValidationError::InvalidEdge {
                    edge_id: edge.edge_id.clone(),
                    message: "publisher edge must have edge_source `publisher`".to_owned(),
                });
            }
        }
        for edge in &self.inferred_edges {
            if edge.edge_source != "inferred" {
                return Err(DecisionValidationError::InvalidEdge {
                    edge_id: edge.edge_id.clone(),
                    message: "inferred edge must have edge_source `inferred`".to_owned(),
                });
            }
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
        if chunk.chunk_builder_version != JURI_DECISION_CHUNK_BUILDER_VERSION {
            return Err(DecisionValidationError::InvalidChunk {
                chunk_id: chunk.chunk_id.clone(),
                message: format!(
                    "chunk_builder_version must be `{JURI_DECISION_CHUNK_BUILDER_VERSION}`"
                ),
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
    #[error(
        "canonical decision source must be a jurisprudence dataset whose family matches the record, got `{dataset}`"
    )]
    InvalidSource { dataset: String },
    #[error("bulk decision chunking_provenance must be `heuristic`, got `{chunking_provenance}`")]
    InvalidChunkingProvenance { chunking_provenance: String },
    #[error("canonical decision canonical_version must be `juri_decision:v1`, got `{canonical_version}`")]
    InvalidCanonicalVersion { canonical_version: String },
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
    #[error("canonical decision edge `{edge_id}` is invalid: {message}")]
    InvalidEdge { edge_id: String, message: String },
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
    #[error("jurisprudence source `{dataset}` does not match XML root family `{root}`")]
    SourceFamilyMismatch {
        dataset: String,
        root: &'static str,
    },
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
    // Reject archive-source/root-family mismatches (e.g. a judicial JURITEXT XML handed to the JADE
    // source) so a record is never misclassified as the wrong official dataset (WARN 4).
    if JuriFamily::for_source(source) != Some(family) {
        return Err(JuriParseError::SourceFamilyMismatch {
            dataset: source.as_str().to_owned(),
            root: family.root_element(),
        });
    }
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
    /// Body text accumulated with inline whitespace collapsed and `\n` at block boundaries.
    body: String,
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
                    // Self-closing block tag (`<br/>`, rarely `<P/>`) inside the body → paragraph
                    // boundary. Gate on the body context exactly like text capture.
                    block if is_body_block_boundary(block) && in_body_context(&stack) => {
                        append_block_boundary(&mut raw.body);
                    }
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                if let Some(name) = stack.last() {
                    // Closing a block element (`</P>`, `</li>`, …) inside the body ends a paragraph.
                    if is_body_block_boundary(name.as_str()) && in_body_context(&stack) {
                        append_block_boundary(&mut raw.body);
                    }
                    if matches!(name.as_str(), "SCT" | "ANA")
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
    // CONTENU body text lives under BLOC_TEXTUEL/CONTENU and may be wrapped in inline/block tags;
    // capture it with inline whitespace collapsing (block boundaries are added on tag start/end).
    if in_body_context(stack) {
        append_xml_content(&mut raw.body, value);
        return;
    }

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
}

/// Whether the current element stack is inside the decision's main text body
/// (`…/BLOC_TEXTUEL/CONTENU/…`). Mirrors the text-capture and `<br/>` guard exactly so they never
/// diverge (NIT 1). `SOMMAIRE` and `CITATION_JP/CONTENU` are excluded because they lack `BLOC_TEXTUEL`.
fn in_body_context(stack: &[String]) -> bool {
    path_contains(stack, &["BLOC_TEXTUEL", "CONTENU"])
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

        let body = finish_body(&self.body);
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
            inferred_edges: Vec::new(),
            chunks: Vec::new(),
            canonical_version: JURI_DECISION_CANONICAL_VERSION.to_owned(),
        };

        decision.publisher_edges = build_publisher_edges(&decision, &self.links);
        decision.inferred_edges = build_inferred_citation_edges(&decision);
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

    for piece in split_body(&decision.body, JURI_DECISION_CHUNK_MAX_CHARS) {
        chunks.push(make_chunk(
            decision,
            &context,
            chunks.len(),
            piece.text,
            "decision_body",
            piece.boundary,
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

/// One body chunk plus an honest boundary marker distinguishing a natural paragraph pack from an
/// emergency size-based split (WARN 5 / ADR fallback-quality case).
struct BodyPiece {
    text: String,
    boundary: &'static str,
}

/// Split body text on paragraph boundaries, packing paragraphs into chunks under `max_chars`.
/// A single over-long paragraph is hard-split on character count as a last resort, and those pieces
/// are labelled `hard_split` so downstream diagnostics can tell them from natural `paragraph` packs.
fn split_body(body: &str, max_chars: usize) -> Vec<BodyPiece> {
    let paragraphs: Vec<&str> = body
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if paragraphs.is_empty() {
        return Vec::new();
    }

    let mut pieces = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, pieces: &mut Vec<BodyPiece>| {
        if !current.is_empty() {
            pieces.push(BodyPiece {
                text: std::mem::take(current),
                boundary: "paragraph",
            });
        }
    };
    for paragraph in paragraphs {
        if paragraph.chars().count() > max_chars {
            flush(&mut current, &mut pieces);
            for text in hard_split(paragraph, max_chars) {
                pieces.push(BodyPiece {
                    text,
                    boundary: "hard_split",
                });
            }
            continue;
        }
        let projected = if current.is_empty() {
            paragraph.chars().count()
        } else {
            current.chars().count() + 1 + paragraph.chars().count()
        };
        if projected > max_chars && !current.is_empty() {
            flush(&mut current, &mut pieces);
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(paragraph);
    }
    flush(&mut current, &mut pieces);
    pieces
}

fn hard_split(text: &str, max_chars: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(max_chars.max(1))
        .map(|chunk| chunk.iter().collect::<String>())
        .collect()
}

/// Max inferred citation edges kept per decision. Decisions citing more distinct articles are rare;
/// this bounds graph bloat while covering the common case.
const MAX_INFERRED_CITATION_EDGES: usize = 64;

// Matches "article(s) <num>" where <num> is an optional L/R/D prefix plus a dotted/hyphenated
// article number (e.g. "L. 1242-14", "R.1332-2", "1014", "1240"). Stops the number at the first
// separator. Case-insensitive.
static ARTICLE_CITATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\barticles?\s+(?P<num>(?:[LRD]\.?\s?)?\d+(?:[-\u{2011}]\d+)*)")
        .expect("valid article citation regex")
});

// Within the short window after an article number (and before the next "article" keyword), detects
// "du [même] code <name>" so a reference can be tied to a statutory code (the signal that
// distinguishes a LEGI article citation from a treaty/convention article).
static CODE_HINT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bdu\s+(?P<same>m[êe]me\s+)?code\b(?P<name>[^.;,\n)]{0,48})")
        .expect("valid code hint regex")
});
static NEXT_ARTICLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\barticles?\b").expect("valid next-article regex"));

/// Parse lower-trust article-citation references from the decision body text into `inferred` graph
/// edges, distinct from official `publisher` `LIEN` edges. To stay precise (and avoid matching
/// treaty/convention articles), a reference is kept only when it carries an `L`/`R`/`D` statutory
/// prefix OR is followed by a "du [même] code …" hint. Targets are NOT resolved here (`to_source_uid`
/// stays `None`); the normalized article number + optional code hint are preserved as evidence.
fn build_inferred_citation_edges(decision: &CanonicalDecision) -> Vec<CanonicalGraphEdge> {
    let body = decision.body.as_str();
    let mut edges = Vec::new();
    let mut seen = BTreeSet::new();

    for capture in ARTICLE_CITATION_RE.captures_iter(body) {
        if edges.len() >= MAX_INFERRED_CITATION_EDGES {
            break;
        }
        let whole = capture.get(0).expect("group 0 always present");
        let raw_num = &capture["num"];
        let normalized = normalize_article_number(raw_num);
        if normalized.is_empty() {
            continue;
        }
        let has_statutory_prefix = matches!(normalized.as_bytes().first(), Some(b'L' | b'R' | b'D'));

        // Look just past the number for a "du [même] code …" hint, but stop at the next "article"
        // keyword so a following reference's code is never mis-attributed to this one. The window end
        // is floored to a UTF-8 char boundary so accented French bodies cannot panic the slice.
        let window = &body[whole.end()..char_safe_window_end(body, whole.end(), 80)];
        let tail = match NEXT_ARTICLE_RE.find(window) {
            Some(next) => &window[..next.start()],
            None => window,
        };
        let code_hint = CODE_HINT_RE.captures(tail).map(|code_capture| {
            if code_capture.name("same").is_some() {
                "même code".to_owned()
            } else {
                let name = code_capture["name"].split_whitespace().collect::<Vec<_>>().join(" ");
                format!("code {name}").trim().to_owned()
            }
        });

        if !has_statutory_prefix && code_hint.is_none() {
            continue; // ambiguous bare number (e.g. "article 8 de la convention") — skip.
        }

        let dedup_key = format!("{normalized}|{}", code_hint.as_deref().unwrap_or(""));
        if !seen.insert(dedup_key) {
            continue;
        }

        let source_text = collapse_ws(whole.as_str());
        let mut attributes = vec![GraphEdgeAttribute {
            key: "article_number".to_owned(),
            value: normalized.clone(),
        }];
        if let Some(code_hint) = &code_hint {
            attributes.push(GraphEdgeAttribute {
                key: "code_hint".to_owned(),
                value: code_hint.clone(),
            });
        }

        edges.push(CanonicalGraphEdge {
            edge_id: inferred_edge_id(&decision.document_id, &normalized, code_hint.as_deref()),
            from_document_id: decision.document_id.clone(),
            from_source_uid: decision.source_uid.clone(),
            to_source_uid: None,
            to_document_id: None,
            relation: "cites_article".to_owned(),
            edge_source: "inferred".to_owned(),
            source_tag: "body_citation".to_owned(),
            source_text: Some(source_text),
            source_payload_hash: decision.source_payload_hash.clone(),
            source_archive: decision.source_archive.clone(),
            source_member_path: decision.source_member_path.clone(),
            attributes,
        });
    }

    edges
}

/// Byte offset `max_bytes` after `start`, clamped to the string length and floored to the nearest
/// UTF-8 char boundary at or after `start`. `start` must already be a char boundary (regex match
/// ends are). This keeps body windowing panic-safe on accented French text.
fn char_safe_window_end(text: &str, start: usize, max_bytes: usize) -> usize {
    let mut end = start.saturating_add(max_bytes).min(text.len());
    while end > start && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Normalize a raw matched article number ("L. 1242-14" → "L1242-14", "R.1332-2" → "R1332-2").
fn normalize_article_number(raw: &str) -> String {
    let mut normalized = String::new();
    for character in raw.chars() {
        match character {
            'l' | 'L' => normalized.push('L'),
            'r' | 'R' => normalized.push('R'),
            'd' | 'D' => normalized.push('D'),
            c if c.is_ascii_digit() => normalized.push(c),
            '-' | '\u{2011}' => normalized.push('-'),
            _ => {} // drop spaces, dots, etc.
        }
    }
    normalized
}

fn inferred_edge_id(from_document_id: &str, article_number: &str, code_hint: Option<&str>) -> String {
    let evidence = format!(
        "{from_document_id}|inferred|cites_article|{article_number}|{}",
        code_hint.unwrap_or_default()
    );
    let hash = source_payload_hash(evidence.as_bytes());
    let digest = hash.strip_prefix("sha256:").unwrap_or(hash.as_str());
    format!("inferred-edge:{digest}")
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

// ----- body assembly (ported from the LEGI CONTENU helpers for identical semantics) ------------

/// Append decision body text, collapsing runs of whitespace to a single space and never emitting a
/// leading space. Block boundaries are inserted separately by [`append_block_boundary`].
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

/// End the current paragraph with a single `\n` (idempotent: never doubles newlines).
fn append_block_boundary(buffer: &mut String) {
    let trimmed_len = buffer.trim_end_matches(' ').len();
    buffer.truncate(trimmed_len);
    if !buffer.is_empty() && !buffer.ends_with('\n') {
        buffer.push('\n');
    }
}

/// XHTML/DILA block tags whose start/end (or self-close) ends a paragraph inside the body.
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

/// Finalize the accumulated body: trim, drop empty lines, and rejoin paragraphs with single `\n`.
fn finish_body(buffer: &str) -> String {
    buffer
        .split('\n')
        .map(collapse_ws)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn path_contains(stack: &[String], needle: &[&str]) -> bool {
    !needle.is_empty()
        && stack.len() >= needle.len()
        && stack
            .windows(needle.len())
            .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
}

// ----- shared small helpers --------------------------------------------------------------------

fn collapse_ws(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
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
    let year = value[0..4].parse::<u16>().unwrap_or_default();
    let month = value[5..7].parse::<u8>().unwrap_or_default();
    let day = value[8..10].parse::<u8>().unwrap_or_default();
    if day > 0 && day <= days_in_month(year, month).unwrap_or_default() {
        Ok(())
    } else {
        Err(())
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

#[cfg(test)]
mod tests;
