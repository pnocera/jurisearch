//! LEGI article chunk building (contextualized + structural body units).

use super::*;

pub(super) const LEGI_ARTICLE_CONTEXTUALIZED_CHUNK_MAX_CHARS: usize = 6_000;

const LEGI_ARTICLE_CHUNK_BUILDER_VERSION: &str = "legi_article_structural:v2";

pub(super) fn build_article_chunks(document: &CanonicalDocument) -> Vec<CanonicalChunk> {
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

pub(super) fn required(
    entity: &'static str,
    field: &'static str,
    value: Option<String>,
) -> Result<String, LegiParseError> {
    let value = value.ok_or(LegiParseError::MissingRequiredField { entity, field })?;
    required_non_empty(entity, field, value)
}

pub(super) fn required_non_empty(
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

pub(super) fn optional_non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    })
}
