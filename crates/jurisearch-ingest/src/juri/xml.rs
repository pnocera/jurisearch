//! Low-level XML helpers for JURI parsing.

use super::*;

/// Append decision body text, collapsing runs of whitespace to a single space and never emitting a
/// leading space. Block boundaries are inserted separately by [`append_block_boundary`].
pub(super) fn append_xml_content(buffer: &mut String, value: &str) {
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
pub(super) fn append_block_boundary(buffer: &mut String) {
    let trimmed_len = buffer.trim_end_matches(' ').len();
    buffer.truncate(trimmed_len);
    if !buffer.is_empty() && !buffer.ends_with('\n') {
        buffer.push('\n');
    }
}

/// XHTML/DILA block tags whose start/end (or self-close) ends a paragraph inside the body.
pub(super) fn is_body_block_boundary(name: &str) -> bool {
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
pub(super) fn finish_body(buffer: &str) -> String {
    buffer
        .split('\n')
        .map(collapse_ws)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn path_contains(stack: &[String], needle: &[&str]) -> bool {
    !needle.is_empty()
        && stack.len() >= needle.len()
        && stack
            .windows(needle.len())
            .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
}

pub(super) fn collapse_ws(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn local_name(name: &[u8]) -> String {
    let name = std::str::from_utf8(name).unwrap_or_default();
    match name.rsplit_once(':') {
        Some((_, local)) => local.to_owned(),
        None => name.to_owned(),
    }
}

/// Resolve a general/character XML entity reference to its text value (predefined entities plus
/// numeric char refs). Mirrors the LEGI parser's handling so decision text decodes identically.
pub(super) fn resolve_reference(reference: &BytesRef<'_>) -> Result<String, JuriParseError> {
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

pub(super) fn collect_attributes(start: &BytesStart<'_>) -> Vec<GraphEdgeAttribute> {
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

pub(super) fn attribute_value(start: &BytesStart<'_>, key: &str) -> Option<String> {
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

pub(super) fn required(
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
