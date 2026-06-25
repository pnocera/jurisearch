//! `fetch` command: return full source text for version-pinned stable IDs, with the
//! optional decision-part overlay delegated to `crate::enrichment`.

use jurisearch_storage::retrieval::{FetchDocumentsQuery, fetch_documents_json};

use crate::*;

pub(crate) fn emit_fetch(args: FetchArgs, index_dir: Option<&Path>) -> anyhow::Result<()> {
    match fetch_payload(args, index_dir) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

pub(crate) fn fetch_payload(args: FetchArgs, index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    let part = match args.part.as_deref() {
        None => None,
        Some(value) => Some(DecisionPart::parse(value).ok_or_else(|| {
            ErrorObject::bad_input(format!(
                "unknown --part `{value}`; expected one of: summary, visa, dispositif, motivations, moyens"
            ))
        })?),
    };
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, QueryReadinessGate::Fetch)?;
    let ids = args.ids.iter().map(String::as_str).collect::<Vec<_>>();
    let response = fetch_documents_json(&postgres, &FetchDocumentsQuery { document_ids: &ids })
        .map_err(storage_error_object)?;
    let mut response: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    if response["documents"]
        .as_array()
        .is_some_and(|documents| documents.is_empty())
    {
        return Err(no_results(
            "fetch returned no documents for the requested IDs",
        ));
    }
    if let Some(part) = part {
        annotate_fetched_parts(&postgres, &mut response, part, args.online)?;
    }
    Ok(response)
}
