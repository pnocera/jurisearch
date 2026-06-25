//! `fetch` command: return full source text for version-pinned stable IDs, with the
//! optional decision-part overlay delegated to `crate::enrichment`.

use jurisearch_storage::retrieval::{FetchDocumentsQuery, fetch_documents_json};

use crate::*;

pub(crate) fn emit_fetch(req: FetchRequest) -> anyhow::Result<()> {
    match fetch_payload(req) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

pub(crate) fn fetch_payload(req: FetchRequest) -> Result<Value, ErrorObject> {
    // Boundary validation shared by the one-shot and session paths.
    if req.ids.is_empty() {
        return Err(ErrorObject::bad_input(
            "fetch requires at least one stable ID",
        ));
    }
    let part = match req.part.as_deref() {
        None => None,
        Some(value) => Some(DecisionPart::parse(value).ok_or_else(|| {
            ErrorObject::bad_input(format!(
                "unknown --part `{value}`; expected one of: summary, visa, dispositif, motivations, moyens"
            ))
        })?),
    };
    let postgres = open_query_index(req.index_dir.as_deref(), QueryReadinessGate::Fetch)?;
    let ids = req.ids.iter().map(String::as_str).collect::<Vec<_>>();
    let response = fetch_documents_json(&postgres, &FetchDocumentsQuery { document_ids: &ids })
        .map_err(storage_error_object)?;
    let mut response: Value = parse_storage_json(&response)?;
    if response["documents"]
        .as_array()
        .is_some_and(|documents| documents.is_empty())
    {
        return Err(no_results(
            "fetch returned no documents for the requested IDs",
        ));
    }
    if let Some(part) = part {
        annotate_fetched_parts(&postgres, &mut response, part, req.online)?;
    }
    Ok(response)
}
