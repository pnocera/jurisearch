//! Publisher links + text-struct link edges and source-uid extraction.

use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawPublisherLink {
    pub(crate) source_tag: String,
    pub(crate) text: String,
    pub(crate) attributes: Vec<GraphEdgeAttribute>,
}

impl RawPublisherLink {
    pub(crate) fn into_edge(self, index: usize, document: &CanonicalDocument) -> CanonicalGraphEdge {
        let to_source_uid = self.target_source_uid();
        let source_text = optional_non_empty(Some(self.text));
        let edge_id = publisher_edge_id(
            document.document_id.as_str(),
            index,
            self.source_tag.as_str(),
            to_source_uid.as_deref(),
            source_text.as_deref(),
        );

        CanonicalGraphEdge {
            edge_id,
            from_document_id: document.document_id.clone(),
            from_source_uid: document.source_uid.clone(),
            to_source_uid,
            to_document_id: None,
            relation: "refers_to".to_owned(),
            edge_source: "publisher".to_owned(),
            source_tag: self.source_tag,
            source_text,
            source_payload_hash: document.source_payload_hash.clone(),
            source_archive: document.source_archive.clone(),
            source_member_path: document.source_member_path.clone(),
            attributes: self.attributes,
        }
    }

    pub(crate) fn target_source_uid(&self) -> Option<String> {
        ["id", "cid", "cidtexte", "href"]
            .iter()
            .find_map(|key| self.attribute_value(key))
            .map(|value| extract_known_source_uid(value.as_str()).unwrap_or(value))
    }

    pub(crate) fn attribute_value(&self, key: &str) -> Option<String> {
        self.attributes
            .iter()
            .find(|attribute| attribute.key == key)
            .and_then(|attribute| optional_non_empty(Some(attribute.value.clone())))
    }
}

pub(crate) fn push_publisher_link(
    raw: &mut RawArticle,
    start: &BytesStart<'_>,
    source_tag: &str,
) -> Result<usize, LegiParseError> {
    let attributes = collect_attributes(start)?;
    raw.publisher_links.push(RawPublisherLink {
        source_tag: source_tag.to_owned(),
        text: String::new(),
        attributes,
    });
    Ok(raw.publisher_links.len() - 1)
}

pub(crate) fn push_text_struct_link(
    raw: &mut RawTextStruct,
    start: &BytesStart<'_>,
    source_tag: &str,
) -> Result<usize, LegiParseError> {
    let attributes = collect_attributes(start)?;
    let target_source_uid = text_struct_link_target_source_uid(&attributes);
    let level =
        text_struct_link_attribute(&attributes, "niv").and_then(|value| value.parse::<i32>().ok());
    let debut = text_struct_link_attribute(&attributes, "debut");
    let fin = text_struct_link_attribute(&attributes, "fin");
    let order = raw.structure_links.len();
    raw.structure_links.push(ParsedTextStructLink {
        source_tag: source_tag.to_owned(),
        order,
        target_source_uid,
        level,
        debut,
        fin,
        text: None,
        attributes,
    });
    Ok(order)
}

pub(crate) fn text_struct_link_attribute(attributes: &[GraphEdgeAttribute], key: &str) -> Option<String> {
    attributes
        .iter()
        .find(|attribute| attribute.key == key)
        .and_then(|attribute| optional_non_empty(Some(attribute.value.clone())))
}

pub(crate) fn text_struct_link_target_source_uid(attributes: &[GraphEdgeAttribute]) -> Option<String> {
    ["id", "cid", "cidtexte", "href"].iter().find_map(|key| {
        text_struct_link_attribute(attributes, key)
            .and_then(|value| extract_known_source_uid(value.as_str()))
    })
}

pub(crate) fn assign_text_struct_link_text(raw: &mut RawTextStruct, link_stack: &[usize], value: &str) {
    let Some(link) = link_stack
        .last()
        .and_then(|index| raw.structure_links.get_mut(*index))
    else {
        return;
    };
    let mut text = link.text.clone().unwrap_or_default();
    append_xml_content(&mut text, value);
    link.text = if text.trim().is_empty() {
        None
    } else {
        Some(text)
    };
}

pub(crate) fn is_text_struct_link_tag(name: &str) -> bool {
    matches!(name, "LIEN_TXT" | "LIEN_SECTION_TA" | "LIEN_ART")
}

pub(crate) fn assign_link_text(raw: &mut RawArticle, link_stack: &[usize], value: &str) {
    if let Some(link) = link_stack
        .last()
        .and_then(|index| raw.publisher_links.get_mut(*index))
    {
        append_xml_content(&mut link.text, value);
    }
}

pub(crate) fn is_publisher_link_tag(name: &str) -> bool {
    matches!(
        name,
        "LIEN" | "LIEN_ART" | "LIEN_SECTION_TA" | "LIEN_TXT" | "a" | "A"
    )
}

pub(crate) fn publisher_edge_id(
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

pub(crate) fn extract_known_source_uid(value: &str) -> Option<String> {
    ["LEGIARTI", "LEGISCTA", "LEGITEXT", "JORFTEXT"]
        .iter()
        .find_map(|prefix| {
            let start = value.find(prefix)?;
            let suffix = value[start + prefix.len()..]
                .chars()
                .take_while(|character| character.is_ascii_digit())
                .take(12)
                .collect::<String>();
            if suffix.len() == 12 {
                Some(format!("{prefix}{suffix}"))
            } else {
                None
            }
        })
}
