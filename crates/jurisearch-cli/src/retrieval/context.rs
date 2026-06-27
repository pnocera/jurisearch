//! context command: structural neighbourhood (ancestry, siblings).

use jurisearch_query::{ContextInput, build_context};
use jurisearch_storage::query::QueryStore;

use crate::*;

pub(crate) fn emit_context(req: ContextRequest) -> anyhow::Result<()> {
    match context_payload(req) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

pub(crate) fn context_payload(req: ContextRequest) -> Result<Value, ErrorObject> {
    let input = resolve_context_input(&req)?;
    let postgres = open_query_index(req.index_dir.as_deref(), QueryReadinessGate::Fetch)?;
    let mut snapshot = postgres.begin_snapshot().map_err(storage_error_object)?;
    build_context(&input, &mut *snapshot)
}

/// Resolve a `context` request into the builder's [`ContextInput`] (boundary validation only, no index).
/// Shared by the CLI adapter and the site context handler.
pub(crate) fn resolve_context_input(req: &ContextRequest) -> Result<ContextInput, ErrorObject> {
    if req.id.trim().is_empty() {
        return Err(ErrorObject::bad_input(
            "context requires a non-empty stable ID",
        ));
    }
    validate_as_of(req.as_of.as_deref())?;
    Ok(ContextInput {
        document_id: req.id.clone(),
        as_of: req.as_of.clone(),
        include_siblings: req.siblings,
    })
}
