//! Low-level XML helpers: path matching, attribute access, body-block boundaries, reference resolution.

use super::*;

pub(super) fn collect_attributes(start: &BytesStart<'_>) -> Result<Vec<GraphEdgeAttribute>, LegiParseError> {
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

pub(super) fn attribute_value(start: &BytesStart<'_>, wanted: &str) -> Result<Option<String>, LegiParseError> {
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

pub(super) fn assign_if_empty(slot: &mut Option<String>, value: &str) {
    if slot.is_none() {
        *slot = Some(value.to_owned());
    }
}

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

pub(super) fn append_block_boundary(buffer: &mut String) {
    let trimmed_len = buffer.trim_end_matches(' ').len();
    buffer.truncate(trimmed_len);
    if !buffer.is_empty() && !buffer.ends_with('\n') {
        buffer.push('\n');
    }
}

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

pub(super) fn resolve_reference(reference: &BytesRef<'_>) -> Result<String, LegiParseError> {
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

pub(super) fn path_ends_with(stack: &[String], tail: &[&str]) -> bool {
    stack.len() >= tail.len()
        && stack[stack.len() - tail.len()..]
            .iter()
            .map(String::as_str)
            .eq(tail.iter().copied())
}

pub(super) fn path_contains(stack: &[String], needle: &[&str]) -> bool {
    !needle.is_empty()
        && stack.len() >= needle.len()
        && stack
            .windows(needle.len())
            .any(|window| window.iter().map(String::as_str).eq(needle.iter().copied()))
}

pub(super) fn local_name(name: &[u8]) -> String {
    String::from_utf8_lossy(name).into_owned()
}

fn attribute_name(name: &[u8]) -> String {
    let name = local_name(name);
    name.rsplit_once(':')
        .map(|(_, local)| local.to_owned())
        .unwrap_or(name)
}
