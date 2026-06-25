//! Top-level one-shot command dispatch: parse the CLI, then route each subcommand to
//! its emitter/payload builder. The command behaviour itself lives in `main.rs` and the
//! command modules; this module only wires parsed args to the right handler.

use clap::Parser;

use jurisearch_core::error::ErrorObject;

use crate::args::*;
use crate::output::*;
use crate::serve::run_serve;
use crate::session::run_jsonl;
use crate::*;

pub(crate) fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let index_dir = cli.index_dir;
    let command = cli.command.unwrap_or(Command::Help(HelpCommand {
        command: Some(HelpSubcommand::Agent),
    }));

    match command {
        Command::Help(help) => emit_help(help),
        Command::Status(args) => write_json(&status_payload(
            index_dir.as_deref(),
            replay_snapshot_mode(args.deep),
        )),
        Command::Session(args) | Command::Batch(args) => run_jsonl(args),
        Command::Serve(args) => run_serve(args, index_dir.as_deref()),
        Command::Ingest(ingest) => emit_ingest(ingest, index_dir.as_deref()),
        Command::Eval(eval) => emit_eval(eval, index_dir.as_deref()),
        Command::Search(args) => {
            if args.query.trim().is_empty() {
                emit_error(ErrorObject::bad_input("search query must not be empty"))
            } else if args.top_k == 0 {
                emit_error(ErrorObject::bad_input("search --top-k must be at least 1"))
            } else {
                emit_search(args, index_dir.as_deref())
            }
        }
        Command::Fetch(args) => {
            if args.ids.is_empty() {
                emit_error(ErrorObject::bad_input(
                    "fetch requires at least one stable ID",
                ))
            } else {
                emit_fetch(args, index_dir.as_deref())
            }
        }
        Command::Cite(args) => {
            if args.cite.trim().is_empty() {
                emit_error(ErrorObject::bad_input("cite requires a non-empty citation"))
            } else {
                emit_cite(args, index_dir.as_deref())
            }
        }
        Command::Related(args) => {
            if args.id.trim().is_empty() {
                emit_error(ErrorObject::bad_input("related requires a document id"))
            } else {
                emit_related(args, index_dir.as_deref())
            }
        }
        Command::Compare(args) => {
            if args.query.trim().is_empty() {
                emit_error(ErrorObject::bad_input("compare query must not be empty"))
            } else if args.top_k == 0 {
                emit_error(ErrorObject::bad_input("compare --top-k must be at least 1"))
            } else {
                emit_compare(args, index_dir.as_deref())
            }
        }
        Command::Context(args) => {
            if args.id.trim().is_empty() {
                emit_error(ErrorObject::bad_input(
                    "context requires a non-empty stable ID",
                ))
            } else {
                emit_context(args, index_dir.as_deref())
            }
        }
        Command::Expand(args) => {
            if args.query.trim().is_empty() {
                emit_error(ErrorObject::bad_input("expand query must not be empty"))
            } else {
                emit_expand(args)
            }
        }
        Command::Model(args) => emit_model(args),
        Command::Setup => write_json(&setup_payload()),
        Command::Doctor => write_json(&doctor_payload(index_dir.as_deref())),
        Command::Stats => match stats_payload(index_dir.as_deref()) {
            Ok(response) => write_json(&response),
            Err(error) => emit_error(error),
        },
        Command::Inspect(args) => {
            if args.id.trim().is_empty() {
                emit_error(ErrorObject::bad_input("inspect requires a document id"))
            } else {
                match inspect_payload(args, index_dir.as_deref()) {
                    Ok(response) => write_json(&response),
                    Err(error) => emit_error(error),
                }
            }
        }
        Command::Versions(args) => {
            if args.id.trim().is_empty() {
                emit_error(ErrorObject::bad_input("versions requires a document id"))
            } else {
                match versions_payload(args, index_dir.as_deref()) {
                    Ok(response) => write_json(&response),
                    Err(error) => emit_error(error),
                }
            }
        }
        Command::Diff(args) => match diff_payload(args, index_dir.as_deref()) {
            Ok(response) => write_json(&response),
            Err(error) => emit_error(error),
        },
        Command::Sync(args) => match sync_payload(args, index_dir.as_deref()) {
            Ok(response) => write_json(&response),
            Err(error) => emit_error(error),
        },
    }
}
