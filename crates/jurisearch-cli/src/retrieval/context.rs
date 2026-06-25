//! context command: structural neighbourhood (ancestry, siblings).

use crate::*;

pub(crate) fn emit_context(args: ContextArgs, index_dir: Option<&Path>) -> anyhow::Result<()> {
    match context_payload(args, index_dir) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

pub(crate) fn context_payload(args: ContextArgs, index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    validate_as_of(args.as_of.as_deref())?;
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, QueryReadinessGate::Fetch)?;
    let response = context_documents_json(
        &postgres,
        &ContextDocumentsQuery {
            document_id: args.id.as_str(),
            as_of: args.as_of.as_deref(),
            include_siblings: args.siblings,
        },
    )
    .map_err(storage_error_object)?;
    let response: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    if response["target"].is_null() {
        Err(no_results(
            "context returned no valid document for the requested ID and --as-of date",
        ))
    } else {
        Ok(response)
    }
}
