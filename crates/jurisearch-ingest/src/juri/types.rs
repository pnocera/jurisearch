//! Parsed JURI domain types (decision family, summary, canonical decision, parse/validation errors).

use super::*;

pub(super) const JURI_DECISION_CANONICAL_VERSION: &str = "juri_decision:v1";

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
    #[error("decision `{source_uid}` has no textual body (empty BLOC_TEXTUEL/CONTENU)")]
    EmptyBody { source_uid: String },
}

impl JuriFamily {
    pub(super) fn entity_name(self) -> &'static str {
        match self {
            JuriFamily::Judicial => "TEXTE_JURI_JUDI",
            JuriFamily::Administrative => "TEXTE_JURI_ADMIN",
        }
    }
}
