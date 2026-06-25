//! `expand` command: curated legal-vocabulary query expansion.

use crate::*;

pub(crate) fn emit_expand(args: QueryArgs) -> anyhow::Result<()> {
    match expand_payload(args) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

pub(crate) fn expand_payload(args: QueryArgs) -> Result<Value, ErrorObject> {
    if args.query.trim().is_empty() {
        return Err(ErrorObject::bad_input("expand query must not be empty"));
    }
    serde_json::to_value(expand_query(&args.query))
        .map_err(|error| dependency_unavailable(error.to_string()))
}
