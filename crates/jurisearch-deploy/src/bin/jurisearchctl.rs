//! `jurisearchctl` — the JuriSearch deployment admin CLI (plan `01-makeitsimpletodeploy`).
//!
//! M1-A implements the `site` config/render verbs (`init`/`config-example`/`validate`/`render`).
//! M4 adds the site-deploy PRODUCT verbs — all WRAPPING the already-built site/syncd/embedder:
//! `site doctor` (distinct-per-class diagnostics, `--json`); `site install` (provision DB + render/install
//! units + bootstrap trust + doctor; REFUSES to start `jurisearch-site` until readiness AND embed doctor
//! are green unless `--force`); `site uninstall|restart|stop|logs|status` (systemd lifecycle);
//! `site bootstrap-trust` (anchors NEVER silently replaced, + license token); `site catch-up` (wrap syncd
//! plan/run catch-up; green only at the verified producer head); `site readiness` (active,
//! readiness-stamped, fingerprint-compatible corpus); `embed doctor|render-service|fetch-assets`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};

use jurisearch_deploy::config::SITE_CONFIG_EXAMPLE;
use jurisearch_deploy::ops::catchup::{CatchupGreen, catch_up_corpus};
use jurisearch_deploy::ops::connection::{read_handle, writer_handle};
use jurisearch_deploy::ops::lifecycle::{
    self, ALL_UNITS, PREREQUISITE_UNITS, StartGate, UNIT_SITE, may_start_site,
};
use jurisearch_deploy::ops::provision::provision_site_db;
use jurisearch_deploy::ops::readiness::readiness_report;
use jurisearch_deploy::ops::trust::bootstrap_trust;
use jurisearch_deploy::ops::{DiagnosticReport, doctor, embed};
use jurisearch_deploy::scaffold::{InitOutcome, init_site_config};
use jurisearch_deploy::{DeployError, SiteConfig};

#[derive(Parser)]
#[command(
    name = "jurisearchctl",
    about = "JuriSearch deployment admin (site config, validation, rendering, doctor, lifecycle)",
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
    /// Local bge-m3 query-embedder operator commands.
    #[command(subcommand)]
    Embed(EmbedCommand),
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
    /// Diagnose prerequisites with a DISTINCT diagnostic per failure class.
    Doctor(DoctorArgs),
    /// Provision DB + render/install units + bootstrap trust; refuses to start the site until ready.
    Install(InstallArgs),
    /// Disable + remove the generated units/env files (never drops DB/corpus/model/site.toml).
    Uninstall(ConfigArgs),
    /// `systemctl restart` the managed units.
    Restart(ConfigArgs),
    /// `systemctl stop` the managed units.
    Stop(ConfigArgs),
    /// `journalctl -u jurisearch-site` (follow).
    Logs(LogsArgs),
    /// `systemctl status` the managed units.
    Status(ConfigArgs),
    /// Install configured trust anchors (never silently replaced) + the license token.
    BootstrapTrust(ConfigArgs),
    /// Plan + apply catch-up for every configured corpus (green only at the verified producer head).
    CatchUp(CatchUpArgs),
    /// Prove the site can answer: active, readiness-stamped, fingerprint-compatible corpus.
    Readiness(ConfigArgs),
}

#[derive(Subcommand)]
enum EmbedCommand {
    /// bge-m3 embedder health + fingerprint compatibility.
    Doctor(DoctorArgs),
    /// Render the bge-m3 systemd service + env file.
    RenderService(RenderArgs),
    /// Fetch model/tokenizer assets via a signed/checksummed manifest (not implemented this release).
    FetchAssets(ConfigArgs),
}

#[derive(Args)]
struct InitArgs {
    #[arg(long, default_value = "/etc/jurisearch/site.toml")]
    config: PathBuf,
}

#[derive(Args)]
struct ConfigArgs {
    #[arg(long)]
    config: PathBuf,
}

#[derive(Args)]
struct RenderArgs {
    #[arg(long)]
    config: PathBuf,
    /// Directory to write rendered files into. When omitted, they are printed (a dry run).
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Args)]
struct DoctorArgs {
    #[arg(long)]
    config: PathBuf,
    /// Emit machine-readable JSON instead of human lines.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct InstallArgs {
    #[arg(long)]
    config: PathBuf,
    /// Write/enable units but start nothing.
    #[arg(long)]
    no_start: bool,
    /// Show all writes/actions without performing them.
    #[arg(long)]
    dry_run: bool,
    /// Skip creating the service user/group.
    #[arg(long)]
    no_user_management: bool,
    /// Start jurisearch-site even if readiness / embed doctor are not green (NOT recommended).
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct CatchUpArgs {
    #[arg(long)]
    config: PathBuf,
    /// Poll/apply until every corpus reaches the verified producer head (or the timeout).
    #[arg(long)]
    wait: bool,
    /// Max seconds to wait per corpus when `--wait` is set.
    #[arg(long, default_value_t = 300)]
    timeout_secs: u64,
}

#[derive(Args)]
struct LogsArgs {
    #[arg(long)]
    config: PathBuf,
    /// Unit to follow (defaults to jurisearch-site.service).
    #[arg(long, default_value = UNIT_SITE)]
    unit: String,
    /// Lines of backlog.
    #[arg(long, default_value_t = 200)]
    lines: u32,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    match cli.command {
        TopCommand::Site(site) => run_site(site),
        TopCommand::Embed(embed) => run_embed(embed),
    }
}

fn run_site(command: SiteCommand) -> Result<ExitCode, String> {
    match command {
        SiteCommand::Init(args) => run_init(&args.config).map(|()| ExitCode::SUCCESS),
        SiteCommand::ConfigExample => {
            print!("{SITE_CONFIG_EXAMPLE}");
            Ok(ExitCode::SUCCESS)
        }
        SiteCommand::Validate(args) => run_validate(&args.config).map(|()| ExitCode::SUCCESS),
        SiteCommand::Render(args) => {
            run_render(&args.config, args.out.as_deref()).map(|()| ExitCode::SUCCESS)
        }
        SiteCommand::Doctor(args) => run_site_doctor(&args),
        SiteCommand::Install(args) => run_install(&args),
        SiteCommand::Uninstall(args) => run_uninstall(&args.config),
        SiteCommand::Restart(args) => {
            run_lifecycle(&args.config, "restart").map(|()| ExitCode::SUCCESS)
        }
        SiteCommand::Stop(args) => run_lifecycle(&args.config, "stop").map(|()| ExitCode::SUCCESS),
        SiteCommand::Logs(args) => run_logs(&args).map(|()| ExitCode::SUCCESS),
        SiteCommand::Status(args) => {
            run_lifecycle(&args.config, "status").map(|()| ExitCode::SUCCESS)
        }
        SiteCommand::BootstrapTrust(args) => {
            run_bootstrap_trust(&args.config).map(|()| ExitCode::SUCCESS)
        }
        SiteCommand::CatchUp(args) => run_catch_up(&args),
        SiteCommand::Readiness(args) => run_readiness(&args.config),
    }
}

fn run_embed(command: EmbedCommand) -> Result<ExitCode, String> {
    match command {
        EmbedCommand::Doctor(args) => run_embed_doctor(&args),
        EmbedCommand::RenderService(args) => {
            run_embed_render(&args.config, args.out.as_deref()).map(|()| ExitCode::SUCCESS)
        }
        EmbedCommand::FetchAssets(_) => Err(format_error(embed::fetch_assets_unimplemented())),
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

fn run_site_doctor(args: &DoctorArgs) -> Result<ExitCode, String> {
    let config = SiteConfig::load(&args.config).map_err(format_error)?;
    let writer = writer_handle(&config);
    let read = read_handle(&config);
    let report = doctor::run_doctor(&config, &writer, &read);
    emit_report(&report, args.json);
    Ok(exit(report.exit_code()))
}

fn run_embed_doctor(args: &DoctorArgs) -> Result<ExitCode, String> {
    let config = SiteConfig::load(&args.config).map_err(format_error)?;
    let writer = writer_handle(&config);
    let report = embed::embed_doctor(&config, Some(&writer));
    emit_report(&report, args.json);
    Ok(exit(report.exit_code()))
}

fn run_embed_render(config: &std::path::Path, out: Option<&std::path::Path>) -> Result<(), String> {
    let parsed = SiteConfig::load(config).map_err(format_error)?;
    let files = embed::render_service(&parsed).map_err(format_error)?;
    match out {
        Some(dir) => {
            // Write ONLY the bge-m3 service + env (preserving modes); never the site/syncd files.
            jurisearch_deploy::render::write_files(dir, &files).map_err(format_error)?;
            for file in &files {
                let mode = if file.secret { "0600" } else { "0644" };
                println!("wrote {}/{} ({mode})", dir.display(), file.relative_path);
            }
            Ok(())
        }
        None => {
            for file in &files {
                println!("# ==> {}", file.relative_path);
                print!("{}", file.contents);
                println!();
            }
            Ok(())
        }
    }
}

fn run_install(args: &InstallArgs) -> Result<ExitCode, String> {
    let config = SiteConfig::load(&args.config).map_err(format_error)?;

    if args.dry_run {
        println!("DRY RUN — no changes will be made:");
        println!(
            "  provision-db: SITE profile on db `{}`",
            config.database.name
        );
        println!(
            "  render env files into {}",
            config.system.config_dir.join("generated").display()
        );
        println!(
            "  install units {} into {}",
            ALL_UNITS.join(", "),
            config.system.systemd_unit_dir.display()
        );
        println!("  bootstrap-trust: {} anchor(s)", config.trust.anchor.len());
        println!(
            "  run doctor; refuse to start jurisearch-site unless readiness + embed doctor green"
        );
        if args.no_start {
            println!("  --no-start: nothing would be started");
        }
        if args.no_user_management {
            println!("  --no-user-management: would not create the service user/group");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // 1. Provision DB (SITE profile).
    let summary = provision_site_db(&config).map_err(format_error)?;
    println!(
        "provisioned db (created={}, schema_version={}, roles={})",
        summary.database_created,
        summary.schema_version,
        summary.roles.join(",")
    );

    // 2. Render env files under config_dir/generated; install the units into the systemd unit dir so
    //    `systemctl daemon-reload`/`enable <unit>` resolve them by bare name.
    let rendered = config.render().map_err(format_error)?;
    rendered
        .write_env_files(&config.system.config_dir)
        .map_err(format_error)?;
    let installed_units = rendered
        .install_units(&config.system.systemd_unit_dir)
        .map_err(format_error)?;
    println!(
        "rendered env into {} and installed {} unit(s) into {}",
        config.system.config_dir.join("generated").display(),
        installed_units.len(),
        config.system.systemd_unit_dir.display()
    );

    // 3. Bootstrap trust (never silently replaced).
    let writer = writer_handle(&config);
    let outcome = bootstrap_trust(&writer, &config).map_err(format_error)?;
    println!(
        "trust bootstrapped (installed={}, unchanged={}, license={})",
        outcome.installed, outcome.unchanged, outcome.license_installed
    );

    // 4. daemon-reload + enable in dependency order.
    lifecycle::run_systemctl(&["daemon-reload".to_owned()]).map_err(format_error)?;
    lifecycle::run_systemctl(&lifecycle::systemctl_argv("enable", &ALL_UNITS))
        .map_err(format_error)?;

    if args.no_start {
        println!("--no-start: units enabled but not started");
        return Ok(ExitCode::SUCCESS);
    }

    // 5. Start prerequisites, then GATE the site start on readiness + embed doctor.
    lifecycle::run_systemctl(&lifecycle::systemctl_argv("start", &PREREQUISITE_UNITS))
        .map_err(format_error)?;

    let read = read_handle(&config);
    let readiness = readiness_report(&writer, &read, &config)
        .map_err(|message| format!("readiness check failed: {message}"))?;
    let embed_report = embed::embed_doctor(&config, Some(&writer));
    let gate = StartGate {
        readiness_green: readiness.is_green(),
        embedder_configured: true,
        embed_doctor_green: embed_report.is_green(),
        force: args.force,
    };
    match may_start_site(&gate) {
        Ok(()) => {
            lifecycle::run_systemctl(&lifecycle::systemctl_argv("start", &[UNIT_SITE]))
                .map_err(format_error)?;
            println!("started {UNIT_SITE}");
            Ok(ExitCode::SUCCESS)
        }
        Err(refusal) => {
            eprintln!("{}", refusal.message());
            print!("{}", readiness.to_lines());
            print!("{}", embed_report.to_lines());
            Ok(exit(1))
        }
    }
}

fn run_uninstall(config: &std::path::Path) -> Result<ExitCode, String> {
    let parsed = SiteConfig::load(config).map_err(format_error)?;
    let unit_dir = &parsed.system.systemd_unit_dir;
    let generated_dir = parsed.system.config_dir.join("generated");
    println!(
        "uninstall: this disables/stops {} (units in {}) and removes their generated env files in {} \
         — it does NOT drop the database, corpus data, package source, model files, or {}.",
        ALL_UNITS.join(", "),
        unit_dir.display(),
        generated_dir.display(),
        config.display()
    );
    // Stop + disable, then remove the installed unit files AND the generated env files for the managed
    // units. Database/corpus/model/site.toml are never touched (plan `01` Phase 4 invariant).
    let _ = lifecycle::run_systemctl(&lifecycle::systemctl_argv("stop", &ALL_UNITS));
    lifecycle::run_systemctl(&lifecycle::systemctl_argv("disable", &ALL_UNITS))
        .map_err(format_error)?;
    for unit in ALL_UNITS {
        let path = unit_dir.join(unit);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove {}: {error}", path.display()))?;
            println!("removed {}", path.display());
        }
    }
    for env in ["site.env", "syncd.env", "bge-m3.env"] {
        let path = generated_dir.join(env);
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|error| format!("remove {}: {error}", path.display()))?;
            println!("removed {}", path.display());
        }
    }
    lifecycle::run_systemctl(&["daemon-reload".to_owned()]).map_err(format_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_lifecycle(config: &std::path::Path, action: &str) -> Result<(), String> {
    let _ = SiteConfig::load(config).map_err(format_error)?;
    lifecycle::run_systemctl(&lifecycle::systemctl_argv(action, &ALL_UNITS)).map_err(format_error)
}

fn run_logs(args: &LogsArgs) -> Result<(), String> {
    let _ = SiteConfig::load(&args.config).map_err(format_error)?;
    lifecycle::run_journalctl(&lifecycle::journalctl_argv(&args.unit, true, args.lines))
        .map_err(format_error)
}

fn run_bootstrap_trust(config: &std::path::Path) -> Result<(), String> {
    let parsed = SiteConfig::load(config).map_err(format_error)?;
    let writer = writer_handle(&parsed);
    let outcome = bootstrap_trust(&writer, &parsed).map_err(format_error)?;
    println!(
        "trust bootstrapped: installed={} unchanged={} license_installed={}",
        outcome.installed, outcome.unchanged, outcome.license_installed
    );
    Ok(())
}

fn run_catch_up(args: &CatchUpArgs) -> Result<ExitCode, String> {
    let config = SiteConfig::load(&args.config).map_err(format_error)?;
    let writer = writer_handle(&config);
    let mut all_green = true;
    for corpus in &config.sync.corpora {
        let result = if args.wait {
            catch_up_with_wait(&writer, &config, corpus, args.timeout_secs)?
        } else {
            catch_up_corpus(&writer, &config, corpus).map_err(format_error)?
        };
        match result.green {
            CatchupGreen::Green => {
                println!("{corpus}: GREEN at head {} ", result.state.head_sequence)
            }
            CatchupGreen::NoActiveCorpus => {
                all_green = false;
                println!("{corpus}: NOT GREEN — no active corpus after catch-up");
            }
            CatchupGreen::NotAtHead { cursor, head } => {
                all_green = false;
                println!("{corpus}: NOT GREEN — cursor {cursor} is not at verified head {head}");
            }
        }
    }
    Ok(exit(u8::from(!all_green)))
}

fn catch_up_with_wait(
    writer: &dyn jurisearch_storage::backend::WriterConnection,
    config: &SiteConfig,
    corpus: &str,
    timeout_secs: u64,
) -> Result<jurisearch_deploy::ops::catchup::CorpusCatchupResult, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    loop {
        let result = catch_up_corpus(writer, config, corpus).map_err(format_error)?;
        if result.green.is_green() || std::time::Instant::now() >= deadline {
            return Ok(result);
        }
        std::thread::sleep(std::time::Duration::from_secs(
            config.sync.interval_secs.max(1),
        ));
    }
}

fn run_readiness(config: &std::path::Path) -> Result<ExitCode, String> {
    let parsed = SiteConfig::load(config).map_err(format_error)?;
    let writer = writer_handle(&parsed);
    let read = read_handle(&parsed);
    let report = readiness_report(&writer, &read, &parsed)
        .map_err(|message| format!("readiness check failed: {message}"))?;
    print!("{}", report.to_lines());
    Ok(exit(report.exit_code()))
}

fn emit_report(report: &DiagnosticReport, json: bool) {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report.to_json()).unwrap_or_else(|_| "{}".to_owned())
        );
    } else {
        print!("{}", report.to_lines());
    }
}

fn exit(code: u8) -> ExitCode {
    ExitCode::from(code)
}

fn mode_label(secret: bool) -> &'static str {
    if secret { "0600" } else { "0644" }
}

fn format_error(error: DeployError) -> String {
    error.to_string()
}
