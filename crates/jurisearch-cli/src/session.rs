//! JSONL session protocol: the transport-neutral `dispatch_session_request` handler, the
//! stdin `run_jsonl` loop, the serde request DTOs (`Session*Args`), and the `session_*_payload`
//! wrappers that adapt each DTO to its one-shot payload builder. Session results are intended
//! to be byte-identical to the corresponding one-shot CLI command.

use std::io;
use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};

use jurisearch_core::contract::{agent_help, command_session_excluded};
use jurisearch_core::error::ErrorObject;
use jurisearch_core::schema::compiled_schema;
use jurisearch_core::session::{SessionRequest, SessionResponse};
use jurisearch_transport::{TransportError, decode_bare_request_line};

use crate::args::*;
use crate::output::write_session_response;
use crate::*;

/// `status` keeps its own small session DTO (option ii): unlike the retrieval/admin commands it has
/// no field duplication to fold into a shared request — the one-shot path reads the global
/// `--index-dir` directly and `status_payload` takes a plain `(index_dir, mode)`.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct SessionStatusArgs {
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
    #[serde(default)]
    pub(crate) deep: bool,
}

pub(crate) fn session_search_payload(args: Value) -> Result<Value, ErrorObject> {
    let req = serde_json::from_value::<SearchRequest>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid search args: {error}")))?;
    search_payload(req)
}

pub(crate) fn session_fetch_payload(args: Value) -> Result<Value, ErrorObject> {
    let req = serde_json::from_value::<FetchRequest>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid fetch args: {error}")))?;
    fetch_payload(req)
}

pub(crate) fn session_cite_payload(args: Value) -> Result<Value, ErrorObject> {
    let req = serde_json::from_value::<CiteRequest>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid cite args: {error}")))?;
    cite_payload(req)
}

pub(crate) fn session_context_payload(args: Value) -> Result<Value, ErrorObject> {
    let req = serde_json::from_value::<ContextRequest>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid context args: {error}")))?;
    context_payload(req)
}

pub(crate) fn session_related_payload(args: Value) -> Result<Value, ErrorObject> {
    let req = serde_json::from_value::<RelatedRequest>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid related args: {error}")))?;
    related_payload(req)
}

pub(crate) fn session_compare_payload(args: Value) -> Result<Value, ErrorObject> {
    let req = serde_json::from_value::<CompareRequest>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid compare args: {error}")))?;
    compare_payload(req)
}

pub(crate) fn session_expand_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<QueryArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid expand args: {error}")))?;
    expand_payload(args)
}

pub(crate) fn session_status_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = if args.is_null() {
        SessionStatusArgs::default()
    } else {
        serde_json::from_value::<SessionStatusArgs>(args)
            .map_err(|error| ErrorObject::bad_input(format!("invalid status args: {error}")))?
    };
    Ok(status_payload(
        args.index_dir.as_deref(),
        replay_snapshot_mode(args.deep),
    ))
}

pub(crate) fn run_jsonl(args: JsonlArgs) -> anyhow::Result<()> {
    if !args.jsonl {
        return emit_error(ErrorObject::bad_input(
            "session and batch require the explicit `--jsonl` flag",
        ));
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.context("failed to read JSONL stdin")?;
        let (response, should_exit) = match decode_bare_request_line(&line) {
            Ok(request) => dispatch_session_request(request),
            Err(error) => {
                // Preserve the exact legacy message bytes ("malformed JSONL request: <serde>").
                let detail = match &error {
                    TransportError::Malformed(inner) => inner.clone(),
                    other => other.to_string(),
                };
                let response = SessionResponse::err(
                    None,
                    ErrorObject::bad_input(format!("malformed JSONL request: {detail}")),
                );
                if args.fatal {
                    write_session_response(&mut stdout, &response)?;
                    break;
                }
                (response, false)
            }
        };
        write_session_response(&mut stdout, &response)?;
        if should_exit {
            break;
        }
    }
    Ok(())
}

pub(crate) fn dispatch_session_request(request: SessionRequest) -> (SessionResponse, bool) {
    let SessionRequest { id, command, args } = request;
    let command = command.trim();
    if command == "exit" {
        return (SessionResponse::ok(id, json!({ "bye": true })), true);
    }
    let result = match command {
        "help" | "help agent" => Ok(json!({ "text": agent_help() })),
        "help schema" | "schema" => Ok(compiled_schema()),
        "status" => session_status_payload(args),
        "search" => session_search_payload(args),
        "fetch" => session_fetch_payload(args),
        "cite" => session_cite_payload(args),
        "context" => session_context_payload(args),
        "related" => session_related_payload(args),
        "compare" => session_compare_payload(args),
        "expand" => session_expand_payload(args),
        "model fetch" => session_model_fetch_payload(args),
        "eval phase1" => session_eval_phase1_payload(args),
        "setup" => Ok(setup_payload()),
        "doctor" => session_doctor_payload(args),
        "stats" => session_stats_payload(args),
        "inspect" => session_inspect_payload(args),
        "versions" => session_versions_payload(args),
        "diff" => session_diff_payload(args),
        // One-shot-only commands (CommandSpec::session_excluded, e.g. `ingest`, `eval france-legi`,
        // `sync`) are advertised but not session-callable: reject with not_implemented so the
        // dispatcher matches the agent contract exactly.
        other if command_session_excluded(other) => Err(ErrorObject::not_implemented(other)),
        _ => Err(ErrorObject::bad_input(format!(
            "unknown session command `{command}`"
        ))),
    };

    match result {
        Ok(result) => (SessionResponse::ok(id, result), false),
        Err(error) => (SessionResponse::err(id, error), false),
    }
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SessionModelFetchArgs {
    pub(crate) model: Option<String>,
    #[serde(default)]
    pub(crate) allow_download: bool,
}

pub(crate) fn session_model_fetch_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionModelFetchArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid model fetch args: {error}")))?;
    model_fetch_payload(args.model, args.allow_download)
}

pub(crate) fn session_eval_phase1_payload(args: Value) -> Result<Value, ErrorObject> {
    let req = serde_json::from_value::<EvalPhase1Request>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid eval phase1 args: {error}")))?;
    eval_phase1_payload(req)
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SessionDoctorArgs {
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

pub(crate) fn session_doctor_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionDoctorArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid doctor args: {error}")))?;
    Ok(doctor_payload(args.index_dir.as_deref()))
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SessionStatsArgs {
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

pub(crate) fn session_stats_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionStatsArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid stats args: {error}")))?;
    stats_payload(args.index_dir.as_deref())
}

pub(crate) fn session_inspect_payload(args: Value) -> Result<Value, ErrorObject> {
    let req = serde_json::from_value::<InspectRequest>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid inspect args: {error}")))?;
    inspect_payload(req)
}

pub(crate) fn session_versions_payload(args: Value) -> Result<Value, ErrorObject> {
    let req = serde_json::from_value::<VersionsRequest>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid versions args: {error}")))?;
    versions_payload(req)
}

pub(crate) fn session_diff_payload(args: Value) -> Result<Value, ErrorObject> {
    let req = serde_json::from_value::<DiffRequest>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid diff args: {error}")))?;
    diff_payload(req)
}
