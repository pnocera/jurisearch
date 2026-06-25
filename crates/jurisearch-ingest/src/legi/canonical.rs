//! Canonical document/chunk/graph-edge output types + validation + source payload hash.

use super::*;

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

pub fn source_payload_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}
