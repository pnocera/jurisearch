//! context command: structural neighbourhood (ancestry, siblings).

use crate::*;

pub(crate) fn emit_context(req: ContextRequest) -> anyhow::Result<()> {
    match context_payload(req) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

pub(crate) fn context_payload(req: ContextRequest) -> Result<Value, ErrorObject> {
    // Boundary validation shared by the one-shot and session paths.
    if req.id.trim().is_empty() {
        return Err(ErrorObject::bad_input(
            "context requires a non-empty stable ID",
        ));
    }
    validate_as_of(req.as_of.as_deref())?;
    let postgres = open_query_index(req.index_dir.as_deref(), QueryReadinessGate::Fetch)?;
    let response = context_documents_json(
        &postgres,
        &ContextDocumentsQuery {
            document_id: req.id.as_str(),
            as_of: req.as_of.as_deref(),
            include_siblings: req.siblings,
        },
    )
    .map_err(storage_error_object)?;
    let response: Value = parse_storage_json(&response)?;
    if response["target"].is_null() {
        Err(no_results(
            "context returned no valid document for the requested ID and --as-of date",
        ))
    } else {
        Ok(response)
    }
}
