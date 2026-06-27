//! `fetch` command: return full source text for version-pinned stable IDs, with the
//! optional decision-part overlay delegated to `crate::enrichment`.

use jurisearch_query::{FetchInput, build_fetch};
use jurisearch_storage::query::QueryStore;

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
    // Adapter owns the side effects (index open + readiness gate); the builder is a pure read over ONE
    // snapshot. work/09 P3B.
    let postgres = open_query_index(req.index_dir.as_deref(), QueryReadinessGate::Fetch)?;
    let mut response = {
        let mut snapshot = postgres.begin_snapshot().map_err(storage_error_object)?;
        build_fetch(
            &FetchInput {
                document_ids: req.ids.clone(),
            },
            &mut *snapshot,
        )?
    };
    // The decision-part overlay (`--part`, online Judilibre) is layered on by the adapter, outside the
    // base read snapshot (it is an online/enrichment concern, not a snapshot read — work/09 P3B scope).
    if let Some(part) = part {
        annotate_fetched_parts(&postgres, &mut response, part, req.online)?;
    }
    Ok(response)
}
