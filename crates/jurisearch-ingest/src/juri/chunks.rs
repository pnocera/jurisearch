//! JURI decision chunk building (heuristic body splitting).

use super::*;

pub(super) const JURI_DECISION_CHUNK_BUILDER_VERSION: &str = "juri_decision_heuristic:v1";

/// Conservative per-chunk character budget for heuristic body splitting. Mirrors the LEGI article
/// contextualized-chunk ceiling so decision chunks stay inside the embedding endpoint budget.
pub(super) const JURI_DECISION_CHUNK_MAX_CHARS: usize = 6_000;

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
pub(super) fn build_decision_chunks(decision: &CanonicalDecision) -> Vec<CanonicalChunk> {
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
