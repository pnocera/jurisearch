//! JSONL session protocol: the transport-neutral `dispatch_session_request` handler, the
//! stdin `run_jsonl` loop, the serde request DTOs (`Session*Args`), and the `session_*_payload`
//! wrappers that adapt each DTO to its one-shot payload builder. Session results are intended
//! to be byte-identical to the corresponding one-shot CLI command.

use std::io;
use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};

use jurisearch_core::contract::{SESSION_EXCLUDED_COMMANDS, agent_help};
use jurisearch_core::error::ErrorObject;
use jurisearch_core::schema::compiled_schema;
use jurisearch_core::session::{SessionRequest, SessionResponse};

use crate::args::*;
use crate::output::write_session_response;
use crate::*;

#[derive(Debug, Deserialize)]
pub(crate) struct SessionSearchArgs {
    pub(crate) query: String,
    #[serde(default = "default_cli_kind")]
    pub(crate) kind: CliKind,
    #[serde(default = "default_search_mode")]
    pub(crate) mode: CliSearchMode,
    #[serde(default = "default_output_format")]
    pub(crate) format: CliOutputFormat,
    #[serde(default = "default_group_by")]
    pub(crate) group_by: CliGroupBy,
    #[serde(default = "default_top_k")]
    pub(crate) top_k: u32,
    #[serde(default)]
    pub(crate) cursor: Option<String>,
    #[serde(default)]
    pub(crate) as_of: Option<String>,
    #[serde(default)]
    pub(crate) rrf_lexical_weight: Option<f64>,
    #[serde(default)]
    pub(crate) rrf_dense_weight: Option<f64>,
    #[serde(default)]
    pub(crate) probes: Option<u32>,
    #[serde(default)]
    pub(crate) court: Option<String>,
    #[serde(default)]
    pub(crate) formation: Option<String>,
    #[serde(default)]
    pub(crate) publication: Option<String>,
    #[serde(default)]
    pub(crate) decided_from: Option<String>,
    #[serde(default)]
    pub(crate) decided_to: Option<String>,
    #[serde(default)]
    pub(crate) zone: Option<CliZone>,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SessionFetchArgs {
    pub(crate) ids: Vec<String>,
    #[serde(default)]
    pub(crate) part: Option<String>,
    #[serde(default)]
    pub(crate) online: bool,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SessionCiteArgs {
    pub(crate) cite: String,
    #[serde(default)]
    pub(crate) strict: bool,
    #[serde(default)]
    pub(crate) online: bool,
    #[serde(default)]
    pub(crate) as_of: Option<String>,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SessionContextArgs {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) siblings: bool,
    #[serde(default)]
    pub(crate) as_of: Option<String>,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SessionRelatedArgs {
    pub(crate) id: String,
    #[serde(default = "default_related_rel")]
    pub(crate) rel: String,
    #[serde(default = "default_related_limit")]
    pub(crate) limit: u32,
    #[serde(default = "default_related_depth")]
    pub(crate) depth: u32,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SessionCompareArgs {
    pub(crate) query: String,
    #[serde(default = "default_compare_kind")]
    pub(crate) kind: CliKind,
    #[serde(default = "default_top_k")]
    pub(crate) top_k: u32,
    #[serde(default)]
    pub(crate) as_of: Option<String>,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
pub(crate) struct SessionStatusArgs {
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
    #[serde(default)]
    pub(crate) deep: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct SessionEvalPhase1Args {
    #[serde(default)]
    pub(crate) list: bool,
    #[serde(default)]
    pub(crate) include_dev: bool,
    #[serde(default = "default_search_mode")]
    pub(crate) mode: CliSearchMode,
    #[serde(default = "default_top_k")]
    pub(crate) top_k: u32,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

pub(crate) fn session_search_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionSearchArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid search args: {error}")))?;
    if args.query.trim().is_empty() {
        return Err(ErrorObject::bad_input("search query must not be empty"));
    }
    if args.top_k == 0 {
        return Err(ErrorObject::bad_input("search top_k must be at least 1"));
    }
    let index_dir = args.index_dir;
    search_payload(
        SearchArgs {
            query: args.query,
            kind: args.kind,
            mode: args.mode,
            format: args.format,
            group_by: args.group_by,
            top_k: args.top_k,
            cursor: args.cursor,
            as_of: args.as_of,
            rrf_lexical_weight: args.rrf_lexical_weight,
            rrf_dense_weight: args.rrf_dense_weight,
            probes: args.probes,
            court: args.court,
            formation: args.formation,
            publication: args.publication,
            decided_from: args.decided_from,
            decided_to: args.decided_to,
            zone: args.zone,
        },
        index_dir.as_deref(),
    )
}

pub(crate) fn session_fetch_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionFetchArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid fetch args: {error}")))?;
    if args.ids.is_empty() {
        return Err(ErrorObject::bad_input(
            "fetch requires at least one stable ID",
        ));
    }
    let index_dir = args.index_dir;
    fetch_payload(
        FetchArgs {
            ids: args.ids,
            part: args.part,
            online: args.online,
        },
        index_dir.as_deref(),
    )
}

pub(crate) fn session_cite_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionCiteArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid cite args: {error}")))?;
    if args.cite.trim().is_empty() {
        return Err(ErrorObject::bad_input("cite requires a non-empty citation"));
    }
    let index_dir = args.index_dir;
    cite_payload(
        CiteArgs {
            cite: args.cite,
            strict: args.strict,
            online: args.online,
            as_of: args.as_of,
        },
        index_dir.as_deref(),
    )
}

pub(crate) fn session_context_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionContextArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid context args: {error}")))?;
    if args.id.trim().is_empty() {
        return Err(ErrorObject::bad_input(
            "context requires a non-empty stable ID",
        ));
    }
    let index_dir = args.index_dir;
    context_payload(
        ContextArgs {
            id: args.id,
            siblings: args.siblings,
            as_of: args.as_of,
        },
        index_dir.as_deref(),
    )
}

pub(crate) fn session_related_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionRelatedArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid related args: {error}")))?;
    if args.id.trim().is_empty() {
        return Err(ErrorObject::bad_input("related requires a non-empty stable ID"));
    }
    let index_dir = args.index_dir;
    related_payload(
        RelatedArgs {
            id: args.id,
            rel: args.rel,
            limit: args.limit,
            depth: args.depth,
        },
        index_dir.as_deref(),
    )
}

pub(crate) fn session_compare_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionCompareArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid compare args: {error}")))?;
    let index_dir = args.index_dir;
    compare_payload(
        CompareArgs {
            query: args.query,
            kind: args.kind,
            top_k: args.top_k,
            as_of: args.as_of,
        },
        index_dir.as_deref(),
    )
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
        let (response, should_exit) = match serde_json::from_str::<SessionRequest>(&line) {
            Ok(request) => dispatch_session_request(request),
            Err(error) => {
                let response = SessionResponse::err(
                    None,
                    ErrorObject::bad_input(format!("malformed JSONL request: {error}")),
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
        // One-shot-only commands (the contract's SESSION_EXCLUDED_COMMANDS, e.g. `related`, `ingest`,
        // `eval france-legi`, `sync`) are advertised but not session-callable: reject with
        // not_implemented so the dispatcher matches the agent contract exactly.
        other if SESSION_EXCLUDED_COMMANDS.contains(&other) => {
            Err(ErrorObject::not_implemented(other))
        }
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
    let args = serde_json::from_value::<SessionEvalPhase1Args>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid eval phase1 args: {error}")))?;
    eval_phase1_payload(
        EvalPhase1Args {
            list: args.list,
            include_dev: args.include_dev,
            mode: args.mode,
            top_k: args.top_k,
        },
        args.index_dir.as_deref(),
    )
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

#[derive(Debug, Deserialize)]
pub(crate) struct SessionInspectArgs {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

pub(crate) fn session_inspect_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionInspectArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid inspect args: {error}")))?;
    if args.id.trim().is_empty() {
        return Err(ErrorObject::bad_input("inspect requires a document id"));
    }
    let index_dir = args.index_dir;
    inspect_payload(InspectArgs { id: args.id }, index_dir.as_deref())
}

#[derive(Debug, Deserialize)]
pub(crate) struct SessionVersionsArgs {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

pub(crate) fn session_versions_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionVersionsArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid versions args: {error}")))?;
    if args.id.trim().is_empty() {
        return Err(ErrorObject::bad_input("versions requires a document id"));
    }
    let index_dir = args.index_dir;
    versions_payload(VersionsArgs { id: args.id }, index_dir.as_deref())
}

#[derive(Debug, Deserialize)]
pub(crate) struct SessionDiffArgs {
    pub(crate) id: String,
    pub(crate) from: String,
    pub(crate) to: String,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

pub(crate) fn session_diff_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionDiffArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid diff args: {error}")))?;
    let index_dir = args.index_dir;
    diff_payload(
        DiffArgs {
            id: args.id,
            from: args.from,
            to: args.to,
        },
        index_dir.as_deref(),
    )
}
