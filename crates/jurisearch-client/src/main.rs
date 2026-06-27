//! work/09 P6 — `jurisearch-client`: the thin client binary. Addressed by a site service URL
//! (`--server tcp://… | unix://…`, `--local`, or `$JURISEARCH_SITE_URL`), it sends one `command` + JSON
//! `args` over the VERSIONED site protocol and renders the response with the SAME bytes the one-shot CLI
//! emits (`jurisearch_render::render_session_response`). It links NONE of the storage/embed/ingest/CLI
//! stack — a thin, structurally-separate artifact a second host runs to query the site.

use std::process::ExitCode;

use clap::Parser;
use jurisearch_client::{resolve_endpoint, send_request};
use jurisearch_core::operation::Operation;
use jurisearch_core::session::SessionRequest;
use jurisearch_render::render_session_response;
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "jurisearch-client",
    about = "JuriSearch thin client — query a site service by URL (versioned site protocol)"
)]
struct Cli {
    /// The site service URL: `tcp://host:port` or `unix:///absolute/path`. Falls back to
    /// `$JURISEARCH_SITE_URL`.
    #[arg(long)]
    server: Option<String>,
    /// Shorthand for a LOCAL `serve-site` over `unix://$XDG_RUNTIME_DIR/jurisearch-site.sock`.
    #[arg(long, conflicts_with = "server")]
    local: bool,
    /// The operation: search / fetch / cite / related / context / compare / status.
    command: String,
    /// The JSON args OBJECT for the operation (e.g. `{"ids":["cass:X"]}`). Defaults to `{}`.
    #[arg(default_value = "{}")]
    args: String,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("jurisearch-client: {message}");
            // Usage/connection/skew failures exit non-zero (distinct from a served error response).
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    let endpoint = resolve_endpoint(cli.server.as_deref(), cli.local).map_err(|e| e.to_string())?;
    let args: serde_json::Value =
        serde_json::from_str(&cli.args).map_err(|error| format!("invalid args JSON: {error}"))?;
    if !args.is_object() {
        return Err("args must be a JSON object (e.g. `{\"ids\":[\"cass:X\"]}`)".to_owned());
    }
    // Validate the command + args against the SAME contract-owned seam the site handlers use
    // (`Operation::parse_args`), so a typo or unsupported field fails FAST locally with the contract's
    // own message instead of a server round-trip. The original `args` are forwarded UNCHANGED — the
    // server re-validates and materializes defaults, so this pre-check never alters the served bytes.
    let operation =
        Operation::parse_command(&cli.command).map_err(|error| error.message.clone())?;
    operation
        .parse_args(&args)
        .map_err(|error| error.message.clone())?;
    let request = SessionRequest {
        id: Some(json!("jurisearch-client")),
        command: cli.command,
        args,
    };
    // A connection/skew failure is a client error (non-zero exit). A SERVED error response is rendered
    // with the same bytes the one-shot CLI emits, and reported via a non-zero exit too.
    let response = send_request(&endpoint, &request).map_err(|e| e.to_string())?;
    let rendered = render_session_response(&response).map_err(|e| e.to_string())?;
    // `render_session_response` already ends with a trailing newline (one-shot byte parity).
    print!("{rendered}");
    Ok(if response.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}
