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
        // Boundary validation (empty query/id, top_k==0, …) now lives once in each payload builder,
        // shared with the session path; the one-shot arms just build the request from the parsed
        // clap args plus the global `--index-dir`.
        Command::Search(args) => emit_search(args.into_request(index_dir)),
        Command::Fetch(args) => emit_fetch(args.into_request(index_dir)),
        Command::Cite(args) => emit_cite(args.into_request(index_dir)),
        Command::Related(args) => emit_related(args.into_request(index_dir)),
        Command::Compare(args) => emit_compare(args.into_request(index_dir)),
        Command::Context(args) => emit_context(args.into_request(index_dir)),
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
        Command::Inspect(args) => match inspect_payload(args.into_request(index_dir)) {
            Ok(response) => write_json(&response),
            Err(error) => emit_error(error),
        },
        Command::Versions(args) => match versions_payload(args.into_request(index_dir)) {
            Ok(response) => write_json(&response),
            Err(error) => emit_error(error),
        },
        Command::Diff(args) => match diff_payload(args.into_request(index_dir)) {
            Ok(response) => write_json(&response),
            Err(error) => emit_error(error),
        },
        Command::Sync(args) => match sync_payload(args, index_dir.as_deref()) {
            Ok(response) => write_json(&response),
            Err(error) => emit_error(error),
        },
    }
}
