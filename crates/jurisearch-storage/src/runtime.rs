use std::{
    fs::{self, File, OpenOptions},
    io,
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
};

use fs2::FileExt;
use tempfile::TempDir;
use thiserror::Error;

const APP_DATABASE: &str = "jurisearch";
const BOOTSTRAP_DATABASE: &str = "postgres";
const SUPERUSER: &str = "postgres";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgConfig {
    pub path: PathBuf,
    pub version: String,
    pub bindir: PathBuf,
    pub pkglibdir: PathBuf,
    pub sharedir: PathBuf,
}

impl PgConfig {
    pub fn discover() -> Result<Self, StorageError> {
        if let Ok(path) = std::env::var("JURISEARCH_PG_CONFIG") {
            return Self::from_path(path);
        }
        if let Ok(path) = std::env::var("PG_CONFIG") {
            return Self::from_path(path);
        }
        Self::from_path(discover_pgrx_pg_config()?)
    }

    pub fn from_path(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let path = path.into();
        if !path.is_file() {
            return Err(StorageError::MissingPgConfig { path });
        }
        let version = command_stdout(&path, ["--version"])?;
        let bindir = PathBuf::from(command_stdout(&path, ["--bindir"])?);
        let pkglibdir = PathBuf::from(command_stdout(&path, ["--pkglibdir"])?);
        let sharedir = PathBuf::from(command_stdout(&path, ["--sharedir"])?);
        Ok(Self {
            path,
            version,
            bindir,
            pkglibdir,
            sharedir,
        })
    }

    pub fn extension_dir(&self) -> PathBuf {
        self.sharedir.join("extension")
    }

    pub fn has_extension_assets(&self, extension: &str) -> bool {
        self.pkglibdir.join(format!("{extension}.so")).is_file()
            && self
                .extension_dir()
                .join(format!("{extension}.control"))
                .is_file()
    }

    pub fn require_extension_assets(&self, extension: &str) -> Result<(), StorageError> {
        if self.has_extension_assets(extension) {
            Ok(())
        } else {
            Err(StorageError::MissingExtensionAssets {
                extension: extension.to_owned(),
                pkglibdir: self.pkglibdir.clone(),
                extension_dir: self.extension_dir(),
            })
        }
    }
}

pub struct ManagedPostgres {
    _temp_dir: Option<TempDir>,
    _startup_lock: Option<StartupLock>,
    advisory_lock: Option<DataDirLock>,
    pub pg_config: PgConfig,
    pub data_dir: PathBuf,
    pub socket_dir: PathBuf,
    pub log_path: PathBuf,
    pub port: u16,
    pub database: String,
}

impl ManagedPostgres {
    pub fn start_temp(pg_config: PgConfig) -> Result<Self, StorageError> {
        pg_config.require_extension_assets("pg_search")?;
        pg_config.require_extension_assets("vector")?;

        let tmp = tempfile::Builder::new()
            .prefix("jurisearch-pg.")
            .tempdir()
            .map_err(StorageError::Io)?;
        let data_dir = tmp.path().join("data");
        let socket_dir = tmp.path().join("sock");
        let log_path = tmp.path().join("postgres.log");
        fs::create_dir_all(&socket_dir).map_err(StorageError::Io)?;
        let port = free_loopback_port()?;

        run_checked(
            pg_config.bindir.join("initdb"),
            vec![
                "-D".into(),
                data_dir.to_string_lossy().into_owned(),
                "--auth=trust".into(),
                "--username=postgres".into(),
            ],
        )?;

        fs::write(
            data_dir.join("postgresql.conf"),
            format!(
                "{}\nshared_preload_libraries = 'pg_search'\nlisten_addresses = '127.0.0.1'\nport = {port}\nunix_socket_directories = '{}'\n",
                fs::read_to_string(data_dir.join("postgresql.conf")).map_err(StorageError::Io)?,
                socket_dir.display()
            ),
        )
        .map_err(StorageError::Io)?;

        let start = Command::new(pg_config.bindir.join("pg_ctl"))
            .arg("-D")
            .arg(&data_dir)
            .arg("-l")
            .arg(&log_path)
            .arg("start")
            .arg("-w")
            .output()
            .map_err(StorageError::Io)?;
        if !start.status.success() {
            return Err(StorageError::PostgresStart {
                status: start.status.code(),
                stderr: String::from_utf8_lossy(&start.stderr).trim().to_owned(),
                log: read_log(&log_path),
            });
        }

        Ok(Self {
            _temp_dir: Some(tmp),
            _startup_lock: None,
            advisory_lock: None,
            pg_config,
            data_dir,
            socket_dir,
            log_path,
            port,
            database: BOOTSTRAP_DATABASE.to_owned(),
        })
    }

    pub fn start_durable(
        pg_config: PgConfig,
        index_dir: impl Into<PathBuf>,
    ) -> Result<Self, StorageError> {
        pg_config.require_extension_assets("pg_search")?;
        pg_config.require_extension_assets("vector")?;

        let index_dir = index_dir.into();
        fs::create_dir_all(&index_dir).map_err(StorageError::Io)?;
        let startup_lock = StartupLock::acquire(index_dir.join("jurisearch-storage.lock"))?;

        let pg_root = index_dir.join("pg");
        let data_dir = pg_root.join("data");
        let socket_dir = pg_root.join("sock");
        let log_path = pg_root.join("postgres.log");
        fs::create_dir_all(&socket_dir).map_err(StorageError::Io)?;
        fs::create_dir_all(&data_dir).map_err(StorageError::Io)?;
        ensure_private_data_dir(&data_dir)?;

        if !data_dir.join("PG_VERSION").is_file() {
            run_checked(
                pg_config.bindir.join("initdb"),
                vec![
                    "-D".into(),
                    data_dir.to_string_lossy().into_owned(),
                    "--auth=trust".into(),
                    "--username=postgres".into(),
                ],
            )?;
        }

        let port = free_loopback_port()?;
        write_runtime_conf(&data_dir, &socket_dir, port)?;
        reclaim_data_dir(&pg_config.bindir, &data_dir);
        start_pg_ctl(&pg_config, &data_dir, &log_path)?;
        ensure_database(&pg_config, port, APP_DATABASE)?;

        let mut postgres = Self {
            _temp_dir: None,
            _startup_lock: Some(startup_lock),
            advisory_lock: None,
            pg_config,
            data_dir,
            socket_dir,
            log_path,
            port,
            database: APP_DATABASE.to_owned(),
        };
        let lock_path = postgres
            .data_dir
            .canonicalize()
            .unwrap_or_else(|_| postgres.data_dir.clone());
        postgres.advisory_lock = Some(DataDirLock::acquire(
            &postgres.connection_string(),
            &lock_path,
        )?);
        postgres.run_migrations()?;
        Ok(postgres)
    }

    #[must_use]
    pub fn connection_string(&self) -> String {
        connection_string(self.port, &self.database)
    }

    pub fn execute_sql(&self, sql: &str) -> Result<String, StorageError> {
        psql(&self.pg_config, self.port, &self.database, sql)
    }

    pub fn stop(&self) -> Result<(), StorageError> {
        let output = Command::new(self.pg_config.bindir.join("pg_ctl"))
            .arg("-D")
            .arg(&self.data_dir)
            .arg("-m")
            .arg("fast")
            .arg("stop")
            .output()
            .map_err(StorageError::Io)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(StorageError::PostgresStop {
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }
}

struct StartupLock {
    file: File,
}

impl StartupLock {
    fn acquire(path: PathBuf) -> Result<Self, StorageError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(StorageError::Io)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(StorageError::Io)?;
        match file.try_lock_exclusive() {
            Ok(()) => Ok(Self { file }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                Err(StorageError::StorageLockBusy { path })
            }
            Err(error) => Err(StorageError::Io(error)),
        }
    }
}

impl Drop for StartupLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

struct DataDirLock {
    client: postgres::Client,
    key: i64,
}

impl DataDirLock {
    fn acquire(connection_string: &str, data_dir: &Path) -> Result<Self, StorageError> {
        let mut client = postgres::Client::connect(connection_string, postgres::NoTls)
            .map_err(StorageError::PostgresClient)?;
        let key = data_dir_lock_key(&data_dir.to_string_lossy());
        let locked: bool = client
            .query_one("SELECT pg_try_advisory_lock($1)", &[&key])
            .map_err(StorageError::PostgresClient)?
            .get(0);
        if locked {
            Ok(Self { client, key })
        } else {
            Err(StorageError::AdvisoryLockBusy {
                data_dir: data_dir.to_path_buf(),
                key,
            })
        }
    }
}

impl Drop for DataDirLock {
    fn drop(&mut self) {
        let _ = self
            .client
            .execute("SELECT pg_advisory_unlock($1)", &[&self.key]);
    }
}

impl Drop for ManagedPostgres {
    fn drop(&mut self) {
        drop(self.advisory_lock.take());
        let _ = self.stop();
    }
}

fn command_stdout<const N: usize>(
    command: &Path,
    args: [&'static str; N],
) -> Result<String, StorageError> {
    let output = Command::new(command)
        .args(args)
        .output()
        .map_err(StorageError::Io)?;
    if !output.status.success() {
        return Err(StorageError::Command {
            command: command.display().to_string(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn psql(
    pg_config: &PgConfig,
    port: u16,
    database: &str,
    sql: &str,
) -> Result<String, StorageError> {
    let port = port.to_string();
    let output = Command::new(pg_config.bindir.join("psql"))
        .args([
            "-h",
            "127.0.0.1",
            "-p",
            &port,
            "-U",
            SUPERUSER,
            "-d",
            database,
            "-v",
            "ON_ERROR_STOP=1",
            "-qAt",
            "-c",
            sql,
        ])
        .output()
        .map_err(StorageError::Io)?;
    if !output.status.success() {
        return Err(StorageError::Psql {
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn start_pg_ctl(
    pg_config: &PgConfig,
    data_dir: &Path,
    log_path: &Path,
) -> Result<(), StorageError> {
    let start = Command::new(pg_config.bindir.join("pg_ctl"))
        .arg("-D")
        .arg(data_dir)
        .arg("-l")
        .arg(log_path)
        .arg("start")
        .arg("-w")
        .output()
        .map_err(StorageError::Io)?;
    if start.status.success() {
        Ok(())
    } else {
        Err(StorageError::PostgresStart {
            status: start.status.code(),
            stderr: String::from_utf8_lossy(&start.stderr).trim().to_owned(),
            log: read_log(log_path),
        })
    }
}

fn ensure_database(pg_config: &PgConfig, port: u16, database: &str) -> Result<(), StorageError> {
    let exists = psql(
        pg_config,
        port,
        BOOTSTRAP_DATABASE,
        &format!(
            "SELECT 1 FROM pg_database WHERE datname = {};",
            sql_string_literal(database)
        ),
    )?;
    if exists.trim() == "1" {
        return Ok(());
    }
    psql(
        pg_config,
        port,
        BOOTSTRAP_DATABASE,
        &format!("CREATE DATABASE {};", sql_identifier(database)),
    )?;
    Ok(())
}

fn write_runtime_conf(data_dir: &Path, socket_dir: &Path, port: u16) -> Result<(), StorageError> {
    let postgresql_conf_path = data_dir.join("postgresql.conf");
    let include_line = "include_if_exists = 'jurisearch.conf'";
    let mut postgresql_conf =
        fs::read_to_string(&postgresql_conf_path).map_err(StorageError::Io)?;
    if !postgresql_conf
        .lines()
        .any(|line| line.trim() == include_line)
    {
        if !postgresql_conf.ends_with('\n') {
            postgresql_conf.push('\n');
        }
        postgresql_conf.push_str(include_line);
        postgresql_conf.push('\n');
        fs::write(&postgresql_conf_path, postgresql_conf).map_err(StorageError::Io)?;
    }

    fs::write(
        data_dir.join("jurisearch.conf"),
        format!(
            "shared_preload_libraries = 'pg_search'\nlisten_addresses = '127.0.0.1'\nport = {port}\nunix_socket_directories = {}\n",
            sql_string_literal(&socket_dir.to_string_lossy())
        ),
    )
    .map_err(StorageError::Io)
}

fn discover_pgrx_pg_config() -> Result<PathBuf, StorageError> {
    let home = std::env::var("HOME").map_err(|_| StorageError::MissingHome)?;
    let pgrx_dir = PathBuf::from(home).join(".pgrx");
    let missing_path = pgrx_dir.join("*/pgrx-install/bin/pg_config");
    let entries = match fs::read_dir(&pgrx_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(StorageError::MissingPgConfig { path: missing_path });
        }
        Err(error) => return Err(StorageError::Io(error)),
    };

    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(StorageError::Io)?;
        let version = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path().join("pgrx-install/bin/pg_config");
        if path.is_file() {
            candidates.push((version_key(&version), path));
        }
    }

    candidates.sort_by(|(left_version, left_path), (right_version, right_path)| {
        left_version
            .cmp(right_version)
            .then_with(|| left_path.cmp(right_path))
    });
    candidates
        .pop()
        .map(|(_, path)| path)
        .ok_or(StorageError::MissingPgConfig { path: missing_path })
}

fn version_key(version: &str) -> Vec<u32> {
    version
        .split(['.', '-'])
        .map(|part| part.parse::<u32>().unwrap_or_default())
        .collect()
}

fn run_checked(command: PathBuf, args: Vec<String>) -> Result<(), StorageError> {
    let output = Command::new(&command)
        .args(args)
        .output()
        .map_err(StorageError::Io)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(StorageError::Command {
            command: command.display().to_string(),
            status: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn reclaim_data_dir(bindir: &Path, data_dir: &Path) {
    let pidfile = data_dir.join("postmaster.pid");
    if !pidfile.exists() {
        return;
    }
    let _ = Command::new(bindir.join("pg_ctl"))
        .arg("-D")
        .arg(data_dir)
        .args(["stop", "-m", "fast", "-t", "20"])
        .output();
    if pidfile.exists() && !postmaster_alive(&pidfile) {
        let _ = fs::remove_file(pidfile);
    }
}

fn postmaster_alive(pidfile: &Path) -> bool {
    let Ok(contents) = fs::read_to_string(pidfile) else {
        return false;
    };
    let Some(pid) = contents
        .lines()
        .next()
        .and_then(|line| line.trim().parse::<u32>().ok())
    else {
        return false;
    };
    #[cfg(target_os = "linux")]
    {
        match fs::read(Path::new("/proc").join(pid.to_string()).join("cmdline")) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).contains("postgres"),
            Err(_) => false,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        true
    }
}

fn ensure_private_data_dir(path: &Path) -> Result<(), StorageError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(StorageError::Io)?;
    }
    Ok(())
}

fn data_dir_lock_key(path: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash as i64
}

fn connection_string(port: u16, database: &str) -> String {
    format!("host=127.0.0.1 port={port} user={SUPERUSER} dbname={database} connect_timeout=5")
}

pub(crate) fn sql_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

pub(crate) fn sql_string_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn free_loopback_port() -> Result<u16, StorageError> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(StorageError::Io)?;
    let port = listener.local_addr().map_err(StorageError::Io)?.port();
    drop(listener);
    Ok(port)
}

fn read_log(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("HOME is not set")]
    MissingHome,
    #[error("pg_config does not exist: {path}")]
    MissingPgConfig { path: PathBuf },
    #[error("another jurisearch process is using this storage root: {path}")]
    StorageLockBusy { path: PathBuf },
    #[error("another session owns the Postgres data-dir advisory lock for {data_dir} ({key})")]
    AdvisoryLockBusy { data_dir: PathBuf, key: i64 },
    #[error(
        "missing extension assets for `{extension}` in pkglibdir={pkglibdir} extension_dir={extension_dir}"
    )]
    MissingExtensionAssets {
        extension: String,
        pkglibdir: PathBuf,
        extension_dir: PathBuf,
    },
    #[error("command `{command}` failed with status {status:?}: {stderr}")]
    Command {
        command: String,
        status: Option<i32>,
        stderr: String,
    },
    #[error("postgres failed to start with status {status:?}: {stderr}\n{log}")]
    PostgresStart {
        status: Option<i32>,
        stderr: String,
        log: String,
    },
    #[error("postgres failed to stop with status {status:?}: {stderr}")]
    PostgresStop { status: Option<i32>, stderr: String },
    #[error("psql failed with status {status:?}: {stderr}")]
    Psql { status: Option<i32>, stderr: String },
    #[error("postgres client error: {0}")]
    PostgresClient(postgres::Error),
    #[error(transparent)]
    Io(io::Error),
}

#[cfg(test)]
mod tests {
    use super::{
        DataDirLock, ManagedPostgres, PgConfig, StorageError, data_dir_lock_key, sql_identifier,
        sql_string_literal, version_key,
    };

    fn discover_pg_config_for_storage_tests() -> Result<Option<PgConfig>, StorageError> {
        let pg_config = match PgConfig::discover() {
            Ok(pg_config) => pg_config,
            Err(error @ StorageError::MissingPgConfig { .. }) => {
                if std::env::var_os("JURISEARCH_REQUIRE_PG_EXTENSIONS").is_some() {
                    return Err(error);
                }
                eprintln!("skipping storage runtime test: {error}");
                return Ok(None);
            }
            Err(error) => return Err(error),
        };

        for extension in ["pg_search", "vector"] {
            if let Err(error) = pg_config.require_extension_assets(extension) {
                if std::env::var_os("JURISEARCH_REQUIRE_PG_EXTENSIONS").is_some() {
                    return Err(error);
                }
                eprintln!("skipping storage runtime test: {error}");
                return Ok(None);
            }
        }

        Ok(Some(pg_config))
    }

    #[test]
    fn pgrx_version_key_keeps_numeric_minor_order() {
        assert!(version_key("18.10") > version_key("18.4"));
        assert!(version_key("19") > version_key("18.10"));
    }

    #[test]
    fn data_dir_lock_key_is_stable() {
        assert_eq!(
            data_dir_lock_key("/tmp/jurisearch/index/pg/data"),
            data_dir_lock_key("/tmp/jurisearch/index/pg/data")
        );
        assert_ne!(
            data_dir_lock_key("/tmp/jurisearch/index/pg/data"),
            data_dir_lock_key("/tmp/jurisearch/other/pg/data")
        );
    }

    #[test]
    fn sql_quoting_escapes_identifiers_and_literals() {
        assert_eq!(sql_identifier("jurisearch"), "\"jurisearch\"");
        assert_eq!(sql_identifier("bad\"name"), "\"bad\"\"name\"");
        assert_eq!(sql_string_literal("sock's"), "'sock''s'");
    }

    #[test]
    fn advisory_lock_rejects_duplicate_session_for_same_data_dir() -> Result<(), StorageError> {
        let Some(pg_config) = discover_pg_config_for_storage_tests()? else {
            return Ok(());
        };
        let root = tempfile::Builder::new()
            .prefix("jurisearch-advisory-lock.")
            .tempdir()
            .map_err(StorageError::Io)?;
        let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
        let lock_path = postgres
            .data_dir
            .canonicalize()
            .unwrap_or_else(|_| postgres.data_dir.clone());

        let duplicate = DataDirLock::acquire(&postgres.connection_string(), &lock_path);
        assert!(matches!(
            duplicate,
            Err(StorageError::AdvisoryLockBusy { .. })
        ));
        Ok(())
    }

    #[test]
    fn durable_start_reclaims_dead_postmaster_pidfile() -> Result<(), StorageError> {
        let Some(pg_config) = discover_pg_config_for_storage_tests()? else {
            return Ok(());
        };
        let root = tempfile::Builder::new()
            .prefix("jurisearch-stale-pid.")
            .tempdir()
            .map_err(StorageError::Io)?;
        let postgres = ManagedPostgres::start_durable(pg_config.clone(), root.path())?;
        let data_dir = postgres.data_dir.clone();
        drop(postgres);

        let stale_pid = "99999999";
        std::fs::write(data_dir.join("postmaster.pid"), format!("{stale_pid}\n"))
            .map_err(StorageError::Io)?;
        let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
        let pidfile = std::fs::read_to_string(postgres.data_dir.join("postmaster.pid"))
            .map_err(StorageError::Io)?;
        assert_ne!(pidfile.lines().next(), Some(stale_pid));
        assert_eq!(postgres.execute_sql("SELECT 1;")?, "1");
        Ok(())
    }
}
