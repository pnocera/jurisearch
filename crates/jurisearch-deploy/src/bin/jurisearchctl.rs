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
use jurisearch_deploy::ops::catchup::{
    CatchupGreen, catch_up_corpus, demo_catchup_blocking_reason,
};
use jurisearch_deploy::ops::connection::{read_handle, writer_handle};
use jurisearch_deploy::ops::lifecycle::{
    self, ALL_UNITS, PREREQUISITE_UNITS, StartGate, UNIT_SITE, may_start_site,
};
use jurisearch_deploy::ops::provision::provision_site_db;
use jurisearch_deploy::ops::readiness::readiness_report;
use jurisearch_deploy::ops::smoke::{SmokePlan, SmokeReport};
use jurisearch_deploy::ops::trust::bootstrap_trust;
use jurisearch_deploy::ops::watchdog::{DEFAULT_STALL_THRESHOLD_SECS, watchdog_corpus};
use jurisearch_deploy::ops::{DiagnosticReport, demo, doctor, embed, fixture, smoke};
use jurisearch_deploy::scaffold::{InitOutcome, init_site_config};
use jurisearch_deploy::{DeployError, SiteConfig};

#[derive(Parser)]
#[command(
    name = "jurisearchctl",
    about = "JuriSearch deployment admin (site config, validation, rendering, doctor, lifecycle)",
    version = jurisearch_buildinfo::version!()
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
    /// Local single-host demo using REAL binaries + the signed FIXTURE corpus (up/url/smoke/down).
    #[command(subcommand)]
    Demo(DemoCommand),
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
    /// Smoke an installed site: status, fetch known id, BM25, hybrid-when-configured, + negative checks.
    Smoke(SmokeArgs),
    /// READ-ONLY watchdog: detect a STALLED site sync cursor, distinct from "no new packages".
    Watchdog(WatchdogArgs),
}

#[derive(Subcommand)]
enum DemoCommand {
    /// Stand up the local demo (provision + trust + catch-up the fixture corpus + gated start).
    Up(InstallArgs),
    /// Print the demo site URL (copy-pasteable into `jurisearch-client --server`).
    Url(ConfigArgs),
    /// Run the demo smoke legs (real status/fetch/search; hybrid skipped-with-reason if assets absent).
    Smoke(SmokeJsonArgs),
    /// Tear the demo down (disable/remove the generated units + env; never drops data).
    Down(ConfigArgs),
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

#[derive(Args)]
struct SmokeArgs {
    #[arg(long)]
    config: PathBuf,
    /// The known document id to fetch (the stable FIXTURE id for demo; a real DILA id for operated).
    #[arg(long, default_value = fixture::FIXTURE_DOC_ID)]
    fetch_id: String,
    /// A query term expected to retrieve at least one candidate.
    #[arg(long, default_value = fixture::FIXTURE_QUERY_TERM)]
    query: String,
    /// A guaranteed-absent id for the negative not-found leg.
    #[arg(long, default_value = fixture::FIXTURE_MISSING_ID)]
    missing_id: String,
    /// Emit machine-readable JSON instead of human lines.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct SmokeJsonArgs {
    #[arg(long)]
    config: PathBuf,
    /// Emit machine-readable JSON instead of human lines.
    #[arg(long)]
    json: bool,
}

#[derive(Args)]
struct WatchdogArgs {
    #[arg(long)]
    config: PathBuf,
    /// A behind cursor older than this (seconds) is reported as STALLED.
    #[arg(long, default_value_t = DEFAULT_STALL_THRESHOLD_SECS)]
    stall_threshold_secs: u64,
    /// Emit machine-readable JSON instead of human lines.
    #[arg(long)]
    json: bool,
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
        TopCommand::Demo(demo) => run_demo(demo),
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
        SiteCommand::Install(args) => run_install(&args, false),
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
        SiteCommand::Smoke(args) => run_site_smoke(&args),
        SiteCommand::Watchdog(args) => run_watchdog(&args),
    }
}

fn run_demo(command: DemoCommand) -> Result<ExitCode, String> {
    match command {
        DemoCommand::Up(args) => run_demo_up(&args),
        DemoCommand::Url(args) => run_demo_url(&args.config),
        DemoCommand::Smoke(args) => run_demo_smoke(&args),
        DemoCommand::Down(args) => run_uninstall(&args.config),
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

/// The bounded ceiling `demo up` waits for each corpus to reach the verified producer head before the
/// readiness gate (mirrors the `site catch-up --wait` default). A corpus still not green after this is a
/// hard `demo up` failure (no start), honouring the command contract.
const DEMO_CATCHUP_TIMEOUT_SECS: u64 = 300;

/// `demo up`: the documented "provision + trust + catch-up the fixture corpus + gated start" sequence.
/// It is `site install` with two demo-specific additions: a FIXTURE PREFLIGHT GUARD up front (fail fast
/// with an actionable diagnostic when the committed fixture bytes are absent) and a SYNCHRONOUS
/// bounded-wait fixture catch-up BEFORE the readiness gate, so the gated start sees the applied corpus
/// rather than depending on asynchronous syncd timing. A corpus still not green after the bounded wait is
/// a HARD failure that refuses the start (no silent proceed) — `site install` (non-demo) keeps its
/// single-pass, async-tolerant behaviour.
fn run_demo_up(args: &InstallArgs) -> Result<ExitCode, String> {
    run_install(args, true)
}

fn run_install(args: &InstallArgs, demo: bool) -> Result<ExitCode, String> {
    let config = SiteConfig::load(&args.config).map_err(format_error)?;

    // FIXTURE PREFLIGHT (demo only): fail fast with one clear diagnostic when the committed fixture
    // artifact is missing, instead of letting the operator fall into a generic catch-up/readiness failure.
    if demo {
        fixture::ensure_published_artifacts(&config).map_err(format_error)?;
    }

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
        if demo {
            println!(
                "  catch-up the fixture corpus ({}) to the verified producer head BEFORE the gate",
                config.sync.corpora.join(", ")
            );
        }
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

    // 5a. DEMO: synchronously catch-up the fixture corpus to the verified producer head BEFORE the
    //     readiness gate, so the gated start reflects the applied fixture rather than depending on async
    //     syncd timing. (The committed-artifact preflight above already guaranteed the bytes are present.)
    //     This is a BOUNDED wait-until-green-or-fail (like `site catch-up --wait`): a non-green corpus
    //     after the bound is a HARD FAILURE that returns before readiness/start, honouring the command
    //     contract that `demo up` catches up to the verified producer head before the gate. (`site install`,
    //     non-demo, keeps its single-pass async-tolerant behaviour and never enters this block.)
    if demo {
        for corpus in &config.sync.corpora {
            let result = catch_up_with_wait(&writer, &config, corpus, DEMO_CATCHUP_TIMEOUT_SECS)?;
            // PURE policy: a corpus still not green after the bounded wait is a HARD failure that returns
            // BEFORE readiness/start (no silent proceed).
            if let Some(reason) =
                demo_catchup_blocking_reason(corpus, result.green, DEMO_CATCHUP_TIMEOUT_SECS)
            {
                return Err(reason);
            }
            println!(
                "demo catch-up: {corpus} GREEN at head {}",
                result.state.head_sequence
            );
        }
    }

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

fn run_site_smoke(args: &SmokeArgs) -> Result<ExitCode, String> {
    let config = SiteConfig::load(&args.config).map_err(format_error)?;
    let endpoint = demo::site_endpoint(&config).map_err(format_error)?;
    // Hybrid runs WHEN configured: the loopback embedder's model + tokenizer assets must be present.
    // When they are not, the hybrid leg is a RECORDED skip — never silently dropped.
    let plan = if demo::model_tokenizer_assets_present(&config) {
        SmokePlan::with_hybrid(&args.fetch_id, &args.query, &args.missing_id)
    } else {
        SmokePlan::without_hybrid(
            &args.fetch_id,
            &args.query,
            &args.missing_id,
            demo::HYBRID_ASSETS_ABSENT_REASON,
        )
    };
    let report = smoke::run_smoke(&endpoint, &plan);
    emit_smoke(&report, args.json)
}

fn run_demo_smoke(args: &SmokeJsonArgs) -> Result<ExitCode, String> {
    let config = SiteConfig::load(&args.config).map_err(format_error)?;
    // The fixture-backed demo smoke fetches the stable fixture id; fail fast with one clear diagnostic if
    // the committed fixture artifact is absent, instead of reporting an obscure fetch/not-found failure.
    fixture::ensure_published_artifacts(&config).map_err(format_error)?;
    let endpoint = demo::site_endpoint(&config).map_err(format_error)?;
    let plan = demo::demo_smoke_plan(&config);
    let report = smoke::run_smoke(&endpoint, &plan);
    emit_smoke(&report, args.json)
}

fn run_demo_url(config: &std::path::Path) -> Result<ExitCode, String> {
    let parsed = SiteConfig::load(config).map_err(format_error)?;
    println!("{}", demo::site_url(&parsed).map_err(format_error)?);
    Ok(ExitCode::SUCCESS)
}

fn emit_smoke(report: &SmokeReport, json: bool) -> Result<ExitCode, String> {
    // The structural acceptance gate: EVERY leg carries an explicit outcome — assert before emitting so
    // a silently-skipped leg can never escape into the operator's report.
    if !report.invariant_no_silent_skip() {
        return Err("internal error: a smoke leg produced no explicit outcome".to_owned());
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report.to_json()).unwrap_or_else(|_| "{}".to_owned())
        );
    } else {
        print!("{}", report.to_lines());
    }
    Ok(exit(report.exit_code()))
}

fn run_watchdog(args: &WatchdogArgs) -> Result<ExitCode, String> {
    let config = SiteConfig::load(&args.config).map_err(format_error)?;
    let writer = writer_handle(&config);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let mut results = Vec::new();
    for corpus in &config.sync.corpora {
        let result = watchdog_corpus(&writer, &config, corpus, now, args.stall_threshold_secs)
            .map_err(format_error)?;
        results.push(result);
    }
    let alert = results.iter().any(|result| result.status.is_alert());
    if args.json {
        let body = serde_json::json!({
            "alert": alert,
            "corpora": results,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&body).unwrap_or_else(|_| "{}".to_owned())
        );
    } else {
        for result in &results {
            println!("{}", result.to_line());
        }
    }
    // The watchdog is READ-ONLY; a non-zero exit only SIGNALS an alert (a stalled/wrong-feed cursor).
    Ok(exit(u8::from(alert)))
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
