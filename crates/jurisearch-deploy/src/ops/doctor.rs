//! `site doctor` (plan `01` Phase 2): one command that explains exactly what is missing, with a DISTINCT
//! diagnostic per failure class — never a generic "not ready".
//!
//! The classes the acceptance gate pins (missing DB, missing extension, stale readiness, occupied bind,
//! missing package manifest, trust/license issues, bad embedder state) are each produced by a PURE
//! classifier keyed to a stable code, unit-tested without live infra. The live orchestrator
//! ([`run_doctor`]) performs the probes (binary/manifest presence on disk; a bind-occupancy probe; a DB
//! reachability + extension + readiness probe; the active trust/corpus topology via syncd) and feeds the
//! classifiers. A connection/probe that genuinely cannot run (no DB at all) surfaces as a `Skipped` check
//! so a structural pre-DB doctor is still meaningful; a DB that is configured-but-down is a `Fail`.

use std::net::TcpListener;
use std::path::Path;

use jurisearch_storage::backend::{ReadHandle, WriterConnection};
use jurisearch_storage::migrations::REQUIRED_EXTENSIONS;
use jurisearch_syncd::{CorpusStatus, corpus_status};

use crate::bind::{BindAddress, parse_bind};
use crate::config::SiteConfig;

use super::readiness::classify_readiness;
use super::{CheckResult, DiagnosticReport};

// ---------------------------------------------------------------------------------------------
// PURE classifiers (one per failure class — distinct stable codes)
// ---------------------------------------------------------------------------------------------

/// The four DB probe outcomes the doctor distinguishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbProbe {
    /// The connection itself failed (server down / wrong host/port/role) — missing DB.
    Unreachable(String),
    /// Connected, but a required extension is absent in the target database.
    MissingExtension(String),
    /// Connected and every required extension is present.
    Reachable,
}

/// Missing DB vs missing extension are DISTINCT diagnostics (acceptance gate).
#[must_use]
pub fn classify_db(probe: &DbProbe) -> CheckResult {
    match probe {
        DbProbe::Unreachable(detail) => CheckResult::fail(
            "db.unreachable",
            format!("the site database is not reachable: {detail}"),
            "provision/start PostgreSQL and check [database] host/port/roles; run `site install`",
        ),
        DbProbe::MissingExtension(name) => CheckResult::fail(
            "db.extension.missing",
            format!("required extension `{name}` is not installed in the target database"),
            format!(
                "install the `{name}` extension (a DBA/superuser `CREATE EXTENSION` may be required)"
            ),
        ),
        DbProbe::Reachable => {
            CheckResult::ok("db.reachable", "database reachable; extensions present")
        }
    }
}

/// A bind-occupancy probe outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindProbe {
    Free,
    Occupied(String),
    /// A unix socket path whose parent directory is missing (cannot bind there yet).
    UnixParentMissing(String),
}

/// Occupied bind is its own distinct diagnostic so it is never confused with a DB/readiness failure.
#[must_use]
pub fn classify_bind(probe: &BindProbe) -> CheckResult {
    match probe {
        BindProbe::Free => CheckResult::ok("bind.free", "the configured site bind address is free"),
        BindProbe::Occupied(detail) => CheckResult::fail(
            "bind.occupied",
            format!("the configured site bind address is already in use: {detail}"),
            "stop whatever holds the port/socket, or change site.bind",
        ),
        BindProbe::UnixParentMissing(path) => CheckResult::fail(
            "bind.unix_parent_missing",
            format!("the unix socket directory for `{path}` does not exist"),
            "create the runtime directory (site install creates RuntimeDirectory=jurisearch)",
        ),
    }
}

/// Check the required role binaries exist + are executable under `install_dir`, plus `llama_server`.
#[must_use]
pub fn check_binaries(install_dir: &Path, llama_server: &Path) -> Vec<CheckResult> {
    let mut results = Vec::new();
    for name in ["jurisearch", "jurisearch-syncd", "jurisearch-client"] {
        let path = install_dir.join(name);
        results.push(binary_check("bin.missing", name, &path));
    }
    results.push(binary_check(
        "bin.llama_server.missing",
        "llama-server",
        llama_server,
    ));
    results
}

fn binary_check(code: &'static str, name: &str, path: &Path) -> CheckResult {
    if is_executable(path) {
        CheckResult::ok(
            "bin.present",
            format!("{name} present at {}", path.display()),
        )
    } else {
        CheckResult::fail(
            code,
            format!(
                "required binary `{name}` is missing or not executable at {}",
                path.display()
            ),
            format!("install `{name}` into the configured install_dir"),
        )
    }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Check `<source_root>/<corpus>/manifest.json` exists for every configured corpus. A missing manifest
/// for a configured corpus is a DISTINCT diagnostic (`sync.manifest.missing`), not a generic network/DB
/// failure (plan `01` Phase 1/2).
#[must_use]
pub fn check_package_manifests(source_root: &Path, corpora: &[String]) -> Vec<CheckResult> {
    corpora
        .iter()
        .map(|corpus| {
            let manifest = source_root.join(corpus).join("manifest.json");
            if manifest.is_file() {
                CheckResult::ok(
                    "sync.manifest.present",
                    format!("package manifest present for corpus `{corpus}`"),
                )
            } else {
                CheckResult::fail(
                    "sync.manifest.missing",
                    format!(
                        "no package manifest for configured corpus `{corpus}` at {}",
                        manifest.display()
                    ),
                    "publish/sync the corpus to the package source_root (this is NOT a network failure)",
                )
            }
        })
        .collect()
}

/// Advisory trust/license status (plan `01` Phase 2: pre-bootstrap doctor may be green with advisory
/// "not yet bootstrapped" statuses; post-bootstrap readiness is the hard gate). `package_anchor_count` /
/// `license_anchor_count` are the INSTALLED counts; a configured-but-not-installed anchor is a WARN, not
/// a FAIL. A configured license token without ANY installed license anchor is a distinct issue.
#[must_use]
pub fn classify_trust(
    package_anchor_count: usize,
    license_configured: bool,
    license_anchor_count: usize,
) -> Vec<CheckResult> {
    let mut results = Vec::new();
    if package_anchor_count == 0 {
        results.push(CheckResult::warn(
            "trust.not_bootstrapped",
            "no package trust anchor is installed yet (not bootstrapped)",
            "run `jurisearchctl site bootstrap-trust --config <path>`",
        ));
    } else {
        results.push(CheckResult::ok(
            "trust.package_anchor.present",
            format!("{package_anchor_count} package trust anchor(s) installed"),
        ));
    }
    if license_configured && license_anchor_count == 0 {
        results.push(CheckResult::warn(
            "license.anchor.not_installed",
            "a license token is configured but no license-purpose trust anchor is installed yet",
            "run `site bootstrap-trust` (it installs the license anchor before the token)",
        ));
    }
    results
}

// ---------------------------------------------------------------------------------------------
// Live orchestrator
// ---------------------------------------------------------------------------------------------

/// The live DB-dependent facts the doctor needs once the database is reachable.
pub struct DoctorDbState {
    pub active: Vec<CorpusStatus>,
    pub readiness_ok: bool,
    pub package_anchor_count: usize,
    pub license_anchor_count: usize,
}

/// The DB-tier outcome fed into [`assemble_doctor_report`]: skipped (no reachable DB), reachable-but the
/// topology/trust read failed, or fully read.
pub enum DoctorDb {
    Skipped,
    TopologyUnreadable(String),
    Ready(DoctorDbState),
}

/// All probe results the doctor report is assembled from. Separating the (pure) assembly from the live
/// probing lets the `run_doctor` WIRING — including the trust and embedder-endpoint diagnostics — be
/// unit-tested with injected probe results, without a live DB / embedder.
pub struct DoctorInputs<'a> {
    pub config: &'a SiteConfig,
    pub binaries: Vec<CheckResult>,
    pub manifests: Vec<CheckResult>,
    pub bind: BindProbe,
    pub structural: Vec<CheckResult>,
    pub endpoint: CheckResult,
    pub db: DbProbe,
    pub db_detail: DoctorDb,
}

/// PURE: assemble the full `site doctor` report from already-collected probe results. Includes the
/// embedder endpoint diagnostic and (when the DB is reachable) the trust/license + readiness +
/// fingerprint diagnostics, so a missing trust anchor or a down/mismatched embedder surfaces in
/// `site doctor`, not only in the standalone classifiers.
#[must_use]
pub fn assemble_doctor_report(inputs: DoctorInputs<'_>) -> DiagnosticReport {
    let DoctorInputs {
        config,
        binaries,
        manifests,
        bind,
        structural,
        endpoint,
        db,
        db_detail,
    } = inputs;
    let mut report = DiagnosticReport::default();

    for check in binaries {
        report.push(check);
    }
    for check in manifests {
        report.push(check);
    }
    report.push(classify_bind(&bind));

    // Embedder structural checks (binary/model/tokenizer presence; loopback) + the live endpoint probe.
    for check in structural {
        report.push(check);
    }
    report.push(endpoint);

    // DB reachability + extensions.
    report.push(classify_db(&db));

    match db_detail {
        DoctorDb::Ready(state) => {
            report.push(classify_readiness(
                config,
                &state.active,
                state.readiness_ok,
            ));
            report.push(active_corpus_note(&state.active));
            // Trust/license diagnostics (advisory pre-bootstrap).
            for check in classify_trust(
                state.package_anchor_count,
                config.license.is_some(),
                state.license_anchor_count,
            ) {
                report.push(check);
            }
            // Embedder fingerprint compatibility against the active corpora.
            report.push(super::embed::classify_fingerprint(config, &state.active));
        }
        DoctorDb::TopologyUnreadable(error) => report.push(CheckResult::fail(
            "topology.unreadable",
            format!("could not read the active corpus topology/trust store: {error}"),
            "check the writer role + jurisearch_control schema",
        )),
        DoctorDb::Skipped => report.push(CheckResult::skipped(
            "trust.readiness.skipped",
            "trust/readiness checks need a reachable database (skipped)",
            "fix db.unreachable first, then re-run doctor",
        )),
    }

    report
}

/// Run the full doctor against `config`. Performs disk/bind probes + the live embedder endpoint probe
/// (always) and DB/trust/readiness probes (live; surfaced as `Skipped` only when no connection can be
/// established at all, `Fail` when configured but down). The same endpoint/fingerprint logic backs both
/// `site doctor` and `embed doctor`, and the trust store is loaded so missing anchors surface here too.
pub fn run_doctor(
    config: &SiteConfig,
    writer: &dyn WriterConnection,
    read: &ReadHandle,
) -> DiagnosticReport {
    let db = probe_db(read);
    let db_detail = if matches!(db, DbProbe::Reachable) {
        match corpus_status(writer) {
            Ok(active) => match super::trust::installed_anchor_counts(writer) {
                Ok((package_anchor_count, license_anchor_count)) => {
                    DoctorDb::Ready(DoctorDbState {
                        active,
                        readiness_ok: read_readiness_ok(read),
                        package_anchor_count,
                        license_anchor_count,
                    })
                }
                Err(error) => DoctorDb::TopologyUnreadable(error.to_string()),
            },
            Err(error) => DoctorDb::TopologyUnreadable(error.to_string()),
        }
    } else {
        DoctorDb::Skipped
    };

    assemble_doctor_report(DoctorInputs {
        config,
        binaries: check_binaries(&config.system.install_dir, &config.embedder.llama_server),
        manifests: check_package_manifests(&config.sync.source_root, &config.sync.corpora),
        bind: probe_bind(config),
        structural: super::embed::structural_checks(config),
        endpoint: super::embed::probe_endpoint(config),
        db,
        db_detail,
    })
}

fn read_readiness_ok(read: &ReadHandle) -> bool {
    read.client()
        .ok()
        .and_then(|mut client| {
            jurisearch_storage::ingest_accounting::load_query_readiness_with_client(&mut client)
                .ok()
        })
        .is_some()
}

fn active_corpus_note(active: &[CorpusStatus]) -> CheckResult {
    if active.is_empty() {
        CheckResult::warn(
            "corpus.none_active",
            "no corpus is active yet (advisory: not yet caught up)",
            "run `site catch-up --config <path> --wait`",
        )
    } else {
        CheckResult::ok(
            "corpus.active",
            format!("{} corpus(es) active", active.len()),
        )
    }
}

/// Probe whether the configured site bind is free.
fn probe_bind(config: &SiteConfig) -> BindProbe {
    match parse_bind(&config.site.bind) {
        Ok(BindAddress::Tcp { host_port, .. }) => match TcpListener::bind(&host_port) {
            Ok(listener) => {
                drop(listener);
                BindProbe::Free
            }
            Err(error) => BindProbe::Occupied(format!("{host_port}: {error}")),
        },
        Ok(BindAddress::Unix { path }) => {
            let parent = Path::new(&path).parent();
            match parent {
                Some(dir) if dir.is_dir() => {
                    if Path::new(&path).exists() {
                        BindProbe::Occupied(format!("{path} already exists"))
                    } else {
                        BindProbe::Free
                    }
                }
                _ => BindProbe::UnixParentMissing(path),
            }
        }
        // A malformed bind is caught by `site validate`; treat it as free here (validation gates first).
        Err(_) => BindProbe::Free,
    }
}

/// Probe DB reachability (read role) + required extensions.
fn probe_db(read: &ReadHandle) -> DbProbe {
    let mut client = match read.client() {
        Ok(client) => client,
        Err(error) => return DbProbe::Unreachable(error.to_string()),
    };
    for extension in REQUIRED_EXTENSIONS {
        let present = client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = $1);",
                &[extension],
            )
            .map(|row| row.get::<_, bool>(0))
            .unwrap_or(false);
        if !present {
            return DbProbe::MissingExtension((*extension).to_owned());
        }
    }
    DbProbe::Reachable
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SITE_CONFIG_EXAMPLE;

    fn example() -> SiteConfig {
        SiteConfig::parse_str(SITE_CONFIG_EXAMPLE, std::path::Path::new("site.toml")).unwrap()
    }

    #[test]
    fn missing_db_and_missing_extension_are_distinct_codes() {
        let unreachable = classify_db(&DbProbe::Unreachable("connection refused".to_owned()));
        let missing_ext = classify_db(&DbProbe::MissingExtension("pg_search".to_owned()));
        assert_eq!(unreachable.code, "db.unreachable");
        assert_eq!(missing_ext.code, "db.extension.missing");
        assert_ne!(unreachable.code, missing_ext.code);
        assert_eq!(unreachable.status, super::super::CheckStatus::Fail);
        assert_eq!(missing_ext.status, super::super::CheckStatus::Fail);
    }

    #[test]
    fn occupied_bind_is_its_own_distinct_diagnostic() {
        let occupied = classify_bind(&BindProbe::Occupied("addr in use".to_owned()));
        assert_eq!(occupied.code, "bind.occupied");
        assert_eq!(occupied.status, super::super::CheckStatus::Fail);
        assert_eq!(
            classify_bind(&BindProbe::Free).status,
            super::super::CheckStatus::Ok
        );
    }

    #[test]
    fn a_missing_package_manifest_is_distinct_from_a_network_failure() {
        let dir = tempfile::tempdir().unwrap();
        let results = check_package_manifests(dir.path(), &["core".to_owned()]);
        assert_eq!(results[0].code, "sync.manifest.missing");
        assert_eq!(results[0].status, super::super::CheckStatus::Fail);
        // Present manifest → ok.
        std::fs::create_dir_all(dir.path().join("core")).unwrap();
        std::fs::write(dir.path().join("core/manifest.json"), "{}").unwrap();
        let results = check_package_manifests(dir.path(), &["core".to_owned()]);
        assert_eq!(results[0].code, "sync.manifest.present");
    }

    #[test]
    fn missing_binaries_are_failures_present_executables_pass() {
        let dir = tempfile::tempdir().unwrap();
        let results = check_binaries(dir.path(), &dir.path().join("llama-server"));
        assert!(
            results
                .iter()
                .all(|r| r.status == super::super::CheckStatus::Fail)
        );
        // Create an executable jurisearch.
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.path().join("jurisearch");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let results = check_binaries(dir.path(), &dir.path().join("llama-server"));
        assert_eq!(results[0].code, "bin.present");
    }

    #[test]
    fn pre_bootstrap_trust_is_advisory_warn_not_fail() {
        let trust = classify_trust(0, true, 0);
        assert_eq!(trust[0].code, "trust.not_bootstrapped");
        assert_eq!(trust[0].status, super::super::CheckStatus::Warn);
        // A configured license without an installed license anchor is a distinct advisory.
        assert!(
            trust
                .iter()
                .any(|c| c.code == "license.anchor.not_installed")
        );
    }

    #[test]
    fn assembled_doctor_surfaces_missing_trust_anchor_and_down_embedder() {
        // Exercise the run_doctor WIRING (assembly seam) — not just the standalone classifiers — so a
        // missing trust anchor AND a down/mismatched embedder endpoint both surface in `site doctor`.
        let config = example();
        let report = assemble_doctor_report(DoctorInputs {
            config: &config,
            binaries: Vec::new(),
            manifests: Vec::new(),
            bind: BindProbe::Free,
            structural: super::super::embed::structural_checks(&config),
            endpoint: CheckResult::fail(
                "embed.endpoint.unreachable",
                "the bge-m3 endpoint did not return a valid embedding",
                "start the bge-m3 endpoint",
            ),
            db: DbProbe::Reachable,
            db_detail: DoctorDb::Ready(DoctorDbState {
                active: Vec::new(),
                readiness_ok: false,
                package_anchor_count: 0,
                license_anchor_count: 0,
            }),
        });
        let codes: Vec<&str> = report.checks.iter().map(|check| check.code).collect();
        // Trust diagnostics are now part of `site doctor`.
        assert!(
            codes.contains(&"trust.not_bootstrapped"),
            "missing package anchor must surface in site doctor, got {codes:?}"
        );
        assert!(codes.contains(&"license.anchor.not_installed"));
        // The live embedder endpoint diagnostic is included.
        assert!(
            codes.contains(&"embed.endpoint.unreachable"),
            "a down embedder must surface in site doctor, got {codes:?}"
        );
        // The embedder fingerprint check ran (no active corpus → advisory).
        assert!(codes.contains(&"embed.fingerprint.no_active_corpus"));
        // Doctor keeps the no-active-corpus readiness ADVISORY (Warn), distinct from the serving gate.
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.code == "readiness.no_active_corpus"
                    && c.status == super::super::CheckStatus::Warn)
        );
    }

    #[test]
    fn assembled_doctor_with_unreachable_db_skips_trust_and_readiness() {
        let config = example();
        let report = assemble_doctor_report(DoctorInputs {
            config: &config,
            binaries: Vec::new(),
            manifests: Vec::new(),
            bind: BindProbe::Free,
            structural: Vec::new(),
            endpoint: CheckResult::ok("embed.dimension.ok", "ok"),
            db: DbProbe::Unreachable("connection refused".to_owned()),
            db_detail: DoctorDb::Skipped,
        });
        let codes: Vec<&str> = report.checks.iter().map(|check| check.code).collect();
        assert!(codes.contains(&"db.unreachable"));
        assert!(codes.contains(&"trust.readiness.skipped"));
        assert!(!codes.contains(&"trust.not_bootstrapped"));
    }

    #[test]
    fn an_occupied_tcp_port_is_detected_by_the_probe() {
        // Bind a loopback port, then point a config at it and confirm the probe reports occupied.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut config = example();
        config.site.bind = format!("tcp://127.0.0.1:{port}");
        assert!(matches!(probe_bind(&config), BindProbe::Occupied(_)));
    }
}
