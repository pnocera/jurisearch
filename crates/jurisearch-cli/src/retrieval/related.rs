//! related command: depth-1 graph neighbours with authority signals.

use crate::*;

pub(crate) fn related_payload(req: RelatedRequest) -> Result<Value, ErrorObject> {
    // Boundary validation shared by the one-shot and session paths.
    if req.id.trim().is_empty() {
        return Err(ErrorObject::bad_input("related requires a document id"));
    }
    if req.depth != 1 {
        return Err(ErrorObject::bad_input(
            "related --depth > 1 is reserved for a later multi-hop graph feature; only depth 1 is supported",
        ));
    }
    if req.rel == "sibling" {
        return Err(ErrorObject::bad_input(
            "related --rel sibling is not a graph relation; use `context --siblings` for structural siblings",
        ));
    }
    let relation = RelatedRelation::parse(&req.rel).ok_or_else(|| {
        ErrorObject::bad_input(format!(
            "unknown --rel `{}`; expected one of: cites, cited_by, temporal, interpreted_by",
            req.rel
        ))
    })?;
    let index_dir = require_existing_index_dir(req.index_dir.as_deref())?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, QueryReadinessGate::Fetch)?;
    let response = related_neighbours_json(
        &postgres,
        &RelatedQuery {
            document_id: &req.id,
            rel: relation,
            limit: req.limit,
        },
    )
    .map_err(storage_error_object)?;
    serde_json::from_str(&response).map_err(|error| dependency_unavailable(error.to_string()))
}

pub(crate) fn emit_related(req: RelatedRequest) -> anyhow::Result<()> {
    match related_payload(req) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}
