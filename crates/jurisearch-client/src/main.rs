//! work/09 P6 — `jurisearch-client`: the thin client binary. Addressed by a site service URL
//! (`--server tcp://… | unix://…`, `--local`, or `$JURISEARCH_SITE_URL`), it sends one `command` + JSON
//! `args` over the VERSIONED site protocol and renders the response with the SAME bytes the one-shot CLI
//! emits (`jurisearch_render::render_session_response`). It links NONE of the storage/embed/ingest/CLI
//! stack — a thin, structurally-separate artifact a second host runs to query the site.
//!
//! M5-A — two RESERVED client-local verbs sit in front of the forwarded site operations:
//!
//! - `configure --server <url>` validates and PERSISTS the site URL to an XDG `client.toml`, so the
//!   thin client points at a site without shell-profile / env editing.
//! - `doctor` checks config presence/parse, endpoint resolution, and a live `status` handshake.
//!
//! Every other `command` is forwarded UNCHANGED to the site, exactly as before; endpoint resolution now
//! also consults the persisted config STRICTLY below `$JURISEARCH_SITE_URL`.

use std::process::ExitCode;

use clap::Parser;
use jurisearch_client::{
    ClientConfig, SiteEndpoint, diagnose_status_probe, endpoint_selector_present, load_config_at,
    resolve_config_path, resolve_endpoint_with_config, save_config_at, send_request,
    status_probe_request,
};
use jurisearch_core::operation::Operation;
use jurisearch_core::session::SessionRequest;
use jurisearch_render::render_session_response;
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "jurisearch-client",
    version = jurisearch_buildinfo::version!(),
    about = "JuriSearch thin client — query a site service by URL (versioned site protocol)"
)]
struct Cli {
    /// The site service URL: `tcp://host:port` or `unix:///absolute/path`. Falls back to
    /// `$JURISEARCH_SITE_URL`, then the `configure`d `client.toml`.
    #[arg(long)]
    server: Option<String>,
    /// Shorthand for a LOCAL `serve-site` over `unix://$XDG_RUNTIME_DIR/jurisearch-site.sock`.
    #[arg(long, conflicts_with = "server")]
    local: bool,
    /// The operation: search / fetch / cite / related / context / compare / status — or a RESERVED
    /// client-local verb: `configure` / `doctor`.
    command: String,
    /// The JSON args OBJECT for the operation (e.g. `{"ids":["cass:X"]}`). Defaults to `{}`. Ignored by
    /// the `configure` / `doctor` verbs.
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
    // The two RESERVED client-local verbs are intercepted BEFORE the site-operation path; everything else
    // is forwarded to the site exactly as before.
    match cli.command.as_str() {
        "configure" => run_configure(&cli),
        "doctor" => run_doctor(&cli),
        _ => run_forward(cli),
    }
}

/// `configure --server <url>` — validate the URL (the SAME grammar the client dials with) and persist it
/// atomically to the XDG `client.toml`. Subsequent invocations resolve this URL when no flag/env wins.
fn run_configure(cli: &Cli) -> Result<ExitCode, String> {
    if cli.local {
        return Err(
            "`configure` persists a durable site URL; `--local` is an ephemeral runtime socket. Pass \
             `--server tcp://host:port` (or `--server unix:///path`)"
                .to_owned(),
        );
    }
    let url = cli.server.as_deref().ok_or_else(|| {
        "`configure` needs a URL to persist: pass `--server tcp://host:port` or \
         `--server unix:///absolute/path`"
            .to_owned()
    })?;
    // Validate via the client's OWN endpoint grammar so we never persist a URL the client cannot dial.
    let endpoint = jurisearch_client::parse_endpoint(url).map_err(|error| error.to_string())?;
    let path = resolve_config_path().ok_or_else(|| {
        "cannot locate a config directory (set $XDG_CONFIG_HOME or $HOME)".to_owned()
    })?;
    save_config_at(
        &path,
        &ClientConfig {
            server: url.to_owned(),
        },
    )
    .map_err(|error| error.to_string())?;
    println!(
        "configured site service {} → {}",
        endpoint.describe(),
        path.display()
    );
    Ok(ExitCode::SUCCESS)
}

/// `doctor` — a short, ordered health report: config presence/parse, endpoint resolution, and a live
/// `status` handshake (connectivity + protocol version + a live query service). Exits 0 only when every
/// check is green; otherwise prints the failing diagnostic and exits 2.
fn run_doctor(cli: &Cli) -> Result<ExitCode, String> {
    let mut healthy = true;
    let mut report = |ok: bool, line: &str| {
        healthy &= ok;
        let mark = if ok { "ok " } else { "FAIL" };
        println!("[{mark}] {line}");
    };

    // A missing config is only a hard failure when NOTHING else resolves an endpoint. When a flag OR
    // `$JURISEARCH_SITE_URL` selects the endpoint, an absent file is purely advisory. We drive this off
    // the SAME selector helper the endpoint-resolution/handshake path uses, so the two cannot disagree.
    let selector_present = endpoint_selector_present(cli.server.as_deref(), cli.local);

    // 1) Config file: present + parseable (an absent file is ADVISORY when a flag/env still resolves).
    let config = match resolve_config_path() {
        Some(path) => match load_config_at(&path) {
            Ok(Some(config)) => {
                report(
                    true,
                    &format!("config {} present ({})", path.display(), config.server),
                );
                Some(config)
            }
            Ok(None) => {
                report(
                    selector_present,
                    &format!(
                        "no client config at {} — run `jurisearch-client configure --server <url>`",
                        path.display()
                    ),
                );
                None
            }
            Err(error) => {
                report(false, &error.to_string());
                None
            }
        },
        None => {
            // No config DIRECTORY resolves (neither $XDG_CONFIG_HOME nor $HOME). This is the SAME
            // missing-config situation as `Ok(None)` and is routed through the SAME advisory decision:
            // ADVISORY (do not flip `healthy`) whenever a flag/env still selects the endpoint, a hard
            // `[FAIL]` only when NOTHING resolves an endpoint.
            report(
                selector_present,
                "no config directory available (set $XDG_CONFIG_HOME or $HOME) — run \
                 `jurisearch-client configure --server <url>`",
            );
            None
        }
    };

    // 2) Endpoint resolution (flags > env > configured URL).
    let endpoint = match resolve_endpoint_with_config(
        cli.server.as_deref(),
        cli.local,
        config.as_ref().map(|c| c.server.as_str()),
    ) {
        Ok(endpoint) => {
            report(
                true,
                &format!("resolved site endpoint {}", endpoint.describe()),
            );
            Some(endpoint)
        }
        Err(error) => {
            report(false, &error.to_string());
            None
        }
    };

    // 3) Live `status` handshake — connectivity + protocol version + a live query service.
    if let Some(endpoint) = endpoint {
        let outcome = send_request(&endpoint, &status_probe_request());
        let (ok, line) = diagnose_status_probe(&endpoint.describe(), &outcome);
        report(ok, &line);
    }

    if healthy {
        println!("doctor: all checks passed");
        Ok(ExitCode::SUCCESS)
    } else {
        // A failed health check is a client-side failure (exit 2), consistent with the binary's contract.
        Err("doctor found one or more problems (see the report above)".to_owned())
    }
}

/// The forwarded site-operation path (unchanged from work/09 P6): resolve the endpoint (now with the
/// `configure`d config as the lowest-priority fallback), pre-validate the command/args against the
/// contract-owned seam, send one request, and render the response with one-shot byte parity.
fn run_forward(cli: Cli) -> Result<ExitCode, String> {
    // Only consult `client.toml` when NO higher-priority selector wins (no `--local`/`--server` and no
    // `$JURISEARCH_SITE_URL`). An explicit/env endpoint outranks the config and must never be hostage to
    // a stale or malformed fallback file — so we don't even read it. A present-but-malformed env var is
    // still an endpoint error (handled inside `resolve_endpoint_with_config`), not a fall-through.
    let configured = if endpoint_selector_present(cli.server.as_deref(), cli.local) {
        None
    } else {
        match resolve_config_path() {
            Some(path) => load_config_at(&path).map_err(|error| error.to_string())?,
            None => None,
        }
    };
    let endpoint: SiteEndpoint = resolve_endpoint_with_config(
        cli.server.as_deref(),
        cli.local,
        configured.as_ref().map(|c| c.server.as_str()),
    )
    .map_err(|e| e.to_string())?;
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
