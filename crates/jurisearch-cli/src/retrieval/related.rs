//! related command: depth-1 graph neighbours with authority signals.

use jurisearch_query::{RelatedInput, build_related};
use jurisearch_storage::query::QueryStore;

use crate::*;

pub(crate) fn related_payload(req: RelatedRequest) -> Result<Value, ErrorObject> {
    let input = resolve_related_input(&req)?;
    let postgres = open_query_index(req.index_dir.as_deref(), QueryReadinessGate::Fetch)?;
    let mut snapshot = postgres.begin_snapshot().map_err(storage_error_object)?;
    build_related(&input, &mut *snapshot)
}

/// Resolve a `related` request into the builder's [`RelatedInput`] (boundary validation only, no index):
/// the depth-1 + non-sibling + known-relation checks. Shared by the CLI adapter and the site handler.
pub(crate) fn resolve_related_input(req: &RelatedRequest) -> Result<RelatedInput, ErrorObject> {
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
    Ok(RelatedInput {
        document_id: req.id.clone(),
        rel: relation,
        limit: req.limit,
    })
}

pub(crate) fn emit_related(req: RelatedRequest) -> anyhow::Result<()> {
    match related_payload(req) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}
