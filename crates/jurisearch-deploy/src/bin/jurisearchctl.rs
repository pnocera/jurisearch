//! `jurisearchctl` — the JuriSearch deployment admin CLI (plan `01-makeitsimpletodeploy`).
//!
//! Milestone M1-A implements the `site` config/render verbs only:
//!   - `site init`           scaffold a site config/dir
//!   - `site config-example` print a complete example site.toml (round-trips through `site validate`)
//!   - `site validate`       strict parse + policy validation with actionable diagnostics
//!   - `site render`         deterministic env + unit rendering
//!
//! Later milestones (M4/M5) add `site doctor/provision-db/install/...` and `embed`/`demo` groups.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use jurisearch_deploy::config::SITE_CONFIG_EXAMPLE;
use jurisearch_deploy::scaffold::{InitOutcome, init_site_config};
use jurisearch_deploy::{DeployError, SiteConfig};

#[derive(Parser)]
#[command(
    name = "jurisearchctl",
    about = "JuriSearch deployment admin (site config, validation, rendering)",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: TopCommand,
}

#[derive(Subcommand)]
enum TopCommand {
    /// Site-host deployment commands.
    #[command(subcommand)]
    Site(SiteCommand),
}

#[derive(Subcommand)]
enum SiteCommand {
    /// Scaffold a site config directory + write a commented template (never clobbers an existing file).
    Init(InitArgs),
    /// Print a complete, commented example site.toml to stdout.
    ConfigExample,
    /// Strict-parse + validate a site.toml, printing actionable diagnostics.
    Validate(ConfigArgs),
    /// Render env files + systemd units deterministically.
    Render(RenderArgs),
}

#[derive(Args)]
struct InitArgs {
    /// Path of the site.toml to create.
    #[arg(long, default_value = "/etc/jurisearch/site.toml")]
    config: PathBuf,
}

#[derive(Args)]
struct ConfigArgs {
    /// Path to the site.toml.
    #[arg(long)]
    config: PathBuf,
}

#[derive(Args)]
struct RenderArgs {
    /// Path to the site.toml.
    #[arg(long)]
    config: PathBuf,
    /// Directory to write rendered `generated/*.env` + `systemd/*.service` files into. When omitted,
    /// rendered files are printed to stdout (a dry run that touches nothing).
    #[arg(long)]
    out: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        TopCommand::Site(site) => run_site(site),
    }
}

fn run_site(command: SiteCommand) -> Result<(), String> {
    match command {
        SiteCommand::Init(args) => run_init(&args.config),
        SiteCommand::ConfigExample => {
            print!("{SITE_CONFIG_EXAMPLE}");
            Ok(())
        }
        SiteCommand::Validate(args) => run_validate(&args.config),
        SiteCommand::Render(args) => run_render(&args.config, args.out.as_deref()),
    }
}

fn run_init(config: &std::path::Path) -> Result<(), String> {
    match init_site_config(config).map_err(format_error)? {
        InitOutcome::Created => {
            println!(
                "created {} (edit it, then `site validate`)",
                config.display()
            );
            Ok(())
        }
        InitOutcome::AlreadyExists => {
            println!("{} already exists; left unchanged", config.display());
            Ok(())
        }
    }
}

fn run_validate(config: &std::path::Path) -> Result<(), String> {
    let parsed = SiteConfig::from_path(config).map_err(format_error)?;
    match parsed.validate() {
        Ok(()) => {
            println!("{}: site config OK", config.display());
            Ok(())
        }
        Err(errors) => Err(format!("{errors}")),
    }
}

fn run_render(config: &std::path::Path, out: Option<&std::path::Path>) -> Result<(), String> {
    let parsed = SiteConfig::load(config).map_err(format_error)?;
    let rendered = parsed.render().map_err(format_error)?;
    match out {
        Some(dir) => {
            rendered.write_to(dir).map_err(format_error)?;
            for file in rendered.files() {
                let mode = if file.secret { "0600" } else { "0644" };
                println!("wrote {}/{} ({mode})", dir.display(), file.relative_path);
            }
            Ok(())
        }
        None => {
            for file in rendered.files() {
                println!("# ==> {} ({})", file.relative_path, mode_label(file.secret));
                print!("{}", file.contents);
                println!();
            }
            Ok(())
        }
    }
}

fn mode_label(secret: bool) -> &'static str {
    if secret { "0600" } else { "0644" }
}

fn format_error(error: DeployError) -> String {
    error.to_string()
}
