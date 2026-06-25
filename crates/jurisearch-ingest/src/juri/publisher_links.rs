//! Publisher link edges (RawLink) and decision/edge id derivation.

use super::*;

pub(crate) struct RawLink {
    pub(crate) text: String,
    pub(crate) attributes: Vec<GraphEdgeAttribute>,
}

/// Build publisher graph edges from `LIENS/LIEN` applied-text references. Bulk/official links are
/// `edge_source = publisher`; targets are resolved when an `id`/`cidtexte` is present, otherwise the
/// raw evidence text is preserved for later resolution.
pub(crate) fn build_publisher_edges(
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

pub(crate) fn link_target_source_uid(attributes: &[GraphEdgeAttribute]) -> Option<String> {
    ["id", "cidtexte"].iter().find_map(|key| {
        attributes
            .iter()
            .find(|attribute| attribute.key.eq_ignore_ascii_case(key))
            .map(|attribute| attribute.value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

pub(crate) fn decision_edge_id(
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
