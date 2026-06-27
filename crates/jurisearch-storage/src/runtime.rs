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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostgresRuntimeProfile {
    Durable,
    BulkIngest,
}

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
        Self::start_durable_with_profile(pg_config, index_dir, PostgresRuntimeProfile::Durable)
    }

    pub fn start_durable_with_profile(
        pg_config: PgConfig,
        index_dir: impl Into<PathBuf>,
        profile: PostgresRuntimeProfile,
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
        write_runtime_conf(&data_dir, &socket_dir, port, profile)?;
        reclaim_data_dir(&pg_config.bindir, &data_dir);
        start_pg_ctl(&pg_config, &data_dir, &log_path)?;
        ensure_database(&pg_config, port, APP_DATABASE)?;
        apply_runtime_profile(&pg_config, port, APP_DATABASE, profile)?;

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

    /// Open a fresh libpq client to this database (the connection dance every module repeats).
    ///
    /// # Errors
    /// [`StorageError::PostgresClient`] if the connection fails.
    pub fn client(&self) -> Result<postgres::Client, StorageError> {
        postgres::Client::connect(&self.connection_string(), postgres::NoTls)
            .map_err(StorageError::PostgresClient)
    }

    /// The server's major version (e.g. `18`), from `server_version_num / 10000`. Used to guard the
    /// `COPY (FORMAT binary)` baseline transport (plan P3 D2): binary COPY is tied to the server's
    /// type layout, so the producer stamps its major into the manifest and the consumer rejects a
    /// mismatch instead of silently corrupting rows.
    ///
    /// # Errors
    /// [`StorageError::PostgresClient`] on a DB error.
    pub fn server_version_major(&self) -> Result<u32, StorageError> {
        let raw = self.execute_sql("SELECT current_setting('server_version_num');")?;
        let num: u32 = raw.trim().parse().map_err(|_| StorageError::Generations {
            message: format!("could not parse server_version_num `{}`", raw.trim()),
        })?;
        Ok(num / 10_000)
    }

    /// Run `sql` with a `search_path` set to `schemas` (each quoted) for this (fresh) `psql` session
    /// only (plan P2). The client read role resolves the active generation per query and sets the
    /// path here, so it can never go stale after a generation switch (unlike `ALTER DATABASE SET
    /// search_path`, which the design rules out, §4.3). Each schema name is quoted with
    /// [`sql_identifier`] so a corpus/generation-derived name cannot break out of the statement.
    pub fn execute_sql_with_search_path(
        &self,
        schemas: &[&str],
        sql: &str,
    ) -> Result<String, StorageError> {
        let path = schemas
            .iter()
            .map(|schema| sql_identifier(schema))
            .collect::<Vec<_>>()
            .join(", ");
        self.execute_sql(&format!("SET search_path TO {path};\n{sql}"))
    }

    /// Run a **read** through the client read-role search path (plan P2; the production retrieval path
    /// uses this so unqualified `documents`/`chunks`/… resolve to the right physical tables):
    /// * no installed corpus (producer or fresh client) → `public` (the authoritative working set);
    /// * exactly one installed corpus → that corpus's **active physical generation** then `public`, so
    ///   BM25/IVFFlat index scans hit the indexed generation tables;
    /// * more than one installed corpus → the `jurisearch_server` UNION views then `public` (correct
    ///   for non-indexed reads; multi-corpus hot indexed search needs per-corpus `UNION ALL` arms — a
    ///   documented follow-up, not yet reachable since only `core` is installed).
    ///
    /// Resolved per call (a fresh `psql` session), so it can never be stale after a generation switch.
    pub fn execute_read_sql(&self, sql: &str) -> Result<String, StorageError> {
        let generations = self.execute_sql(
            "SELECT coalesce(string_agg('jurisearch_server_' || active_generation, ',' \
                 ORDER BY corpus), '') FROM jurisearch_control.corpus_state;",
        )?;
        let active: Vec<&str> = generations
            .trim()
            .split(',')
            .filter(|schema| !schema.is_empty())
            .collect();
        match active.len() {
            0 => self.execute_sql_with_search_path(&["public"], sql),
            1 => self.execute_sql_with_search_path(&[active[0], "public"], sql),
            _ => self.execute_sql_with_search_path(&["jurisearch_server", "public"], sql),
        }
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

/// Deliberately conservative common LOWER bound for every overridable memory GUC, in bytes (1 MiB).
/// It sits comfortably above the actual per-GUC minimums Postgres enforces (all ≤ ~1 MB), so a single
/// floor keeps EVERY override startup-safe: a value below it is rejected and falls back to the
/// conservative default rather than producing a `jurisearch.conf` Postgres refuses to start with.
const MIN_PG_MEM_BYTES: u64 = 1024 * 1024;
/// Common UPPER bound for every overridable memory GUC, in bytes. The tightest max among the exposed
/// memory GUCs is the `work_mem`/`maintenance_work_mem` family's `MAX_KILOBYTES` (`INT_MAX` kB); the
/// other exposed GUCs (`shared_buffers`, `effective_cache_size`, `temp_buffers`) allow strictly more,
/// so a value at/below this is startup-safe for ALL of them. Above it — e.g. `2TB`/`16TB` — is rejected
/// and falls back to the default instead of writing a literal Postgres rejects at startup.
const MAX_PG_MEM_BYTES: u64 = (i32::MAX as u64) * 1024;
/// Upper bound for the overridable `*_parallel_*_workers` GUCs (Postgres caps each at 1024). A larger
/// value — or one that overflows — is rejected so a hostile override cannot break startup.
const MAX_PG_PARALLEL_WORKERS: u32 = 1024;

/// Resolve a managed-Postgres tuning value: the `JURISEARCH_PG_*` override in `var` if present AND it
/// passes `valid`, otherwise `default`. Thin env wrapper over [`resolve_pg_setting`] (which holds the
/// testable logic, so the validation/fallback contract is covered without mutating process env).
fn pg_runtime_setting(var: &str, default: &str, valid: fn(&str) -> bool) -> String {
    resolve_pg_setting(std::env::var(var).ok().as_deref(), default, valid)
}

/// Choose `override_value` (trimmed) when present AND it passes `valid`, else `default`. The result is
/// written verbatim into `jurisearch.conf`, so `valid` MUST reject anything Postgres would choke on —
/// a malformed, out-of-range, or hostile override silently falls back to the conservative default
/// instead of injecting config or breaking startup.
fn resolve_pg_setting(
    override_value: Option<&str>,
    default: &str,
    valid: fn(&str) -> bool,
) -> String {
    override_value
        .map(str::trim)
        .filter(|value| valid(value))
        .map_or_else(|| default.to_owned(), str::to_owned)
}

/// Parse a Postgres memory literal — `<number><unit>` with a plain decimal number (no sign/exponent)
/// and a required `B`/`kB`/`MB`/`GB`/`TB` unit (e.g. `256MB`, `1.5GB`) — into a byte count. Returns
/// `None` on any malformed input or on overflow, so it doubles as the injection/overflow guard.
fn parse_pg_bytes(value: &str) -> Option<u64> {
    let unit_start = value.find(|c: char| c.is_ascii_alphabetic())?;
    let (number, unit) = value.split_at(unit_start);
    if !is_plain_decimal(number) {
        return None;
    }
    let number: f64 = number.parse().ok()?;
    let multiplier: u64 = match unit {
        "B" => 1,
        "kB" => 1 << 10,
        "MB" => 1 << 20,
        "GB" => 1 << 30,
        "TB" => 1 << 40,
        _ => return None,
    };
    let bytes = number * multiplier as f64;
    (bytes.is_finite() && (0.0..=u64::MAX as f64).contains(&bytes)).then_some(bytes as u64)
}

/// A plain non-negative decimal: digits, optionally one `.` with a fractional part. No sign, exponent,
/// or stray characters — so `f64::parse` afterwards can only see a value Postgres also accepts.
fn is_plain_decimal(value: &str) -> bool {
    let mut parts = value.split('.');
    let integer = parts.next().unwrap_or_default();
    let fraction = parts.next();
    parts.next().is_none()
        && !integer.is_empty()
        && integer.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.is_none_or(|frac| !frac.is_empty() && frac.bytes().all(|b| b.is_ascii_digit()))
}

/// A memory override Postgres will accept AND start with: a well-formed literal whose size is within
/// `[MIN_PG_MEM_BYTES, MAX_PG_MEM_BYTES]`. Rejects `0`-sized/sub-floor values and over-max values (both
/// fail startup) and anything unparseable.
fn is_pg_mem_literal(value: &str) -> bool {
    parse_pg_bytes(value)
        .is_some_and(|bytes| (MIN_PG_MEM_BYTES..=MAX_PG_MEM_BYTES).contains(&bytes))
}

/// A worker-count override: a bare non-negative integer within `[0, MAX_PG_PARALLEL_WORKERS]`. The
/// leading ASCII-digit check rejects signs (incl. the `+` Rust's `parse` would otherwise accept) and
/// units; `parse::<u32>` then rejects overflow; the bound rejects out-of-range counts.
fn is_pg_int_literal(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value
            .parse::<u32>()
            .is_ok_and(|count| count <= MAX_PG_PARALLEL_WORKERS)
}

fn write_runtime_conf(
    data_dir: &Path,
    socket_dir: &Path,
    port: u16,
    profile: PostgresRuntimeProfile,
) -> Result<(), StorageError> {
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

    let mut runtime_conf = format!(
        "shared_preload_libraries = 'pg_search'\nlisten_addresses = '127.0.0.1'\nport = {port}\nunix_socket_directories = {}\n",
        sql_string_literal(&socket_dir.to_string_lossy())
    );
    // Analytical/parallel knobs applied to both profiles. Defaults are deliberately CONSERVATIVE: a
    // jurisearch client is typically a modest machine that co-hosts local LLM/embedding services, so
    // the managed Postgres must not claim a large buffer pool or big per-op work_mem by default. Every
    // knob is overridable via a `JURISEARCH_PG_*` env var (validated to a safe Postgres literal so an
    // override can never inject arbitrary config into the conf file) — that is how a dedicated server
    // (e.g. bear) opts into aggressive values, rather than the code defaulting to them. Stock Postgres
    // (work_mem=4MB, no parallelism) made the France-LEGI gold CTEs and BM25/vector fusion spill to
    // disk and run single-threaded, so these sit above stock but well below a 25%-of-RAM profile.
    let effective_cache_size = pg_runtime_setting(
        "JURISEARCH_PG_EFFECTIVE_CACHE_SIZE",
        "2GB",
        is_pg_mem_literal,
    );
    let work_mem = pg_runtime_setting("JURISEARCH_PG_WORK_MEM", "64MB", is_pg_mem_literal);
    let maintenance_work_mem = pg_runtime_setting(
        "JURISEARCH_PG_MAINTENANCE_WORK_MEM",
        "256MB",
        is_pg_mem_literal,
    );
    let temp_buffers = pg_runtime_setting("JURISEARCH_PG_TEMP_BUFFERS", "32MB", is_pg_mem_literal);
    let max_parallel_workers_per_gather = pg_runtime_setting(
        "JURISEARCH_PG_MAX_PARALLEL_WORKERS_PER_GATHER",
        "2",
        is_pg_int_literal,
    );
    let max_parallel_workers =
        pg_runtime_setting("JURISEARCH_PG_MAX_PARALLEL_WORKERS", "4", is_pg_int_literal);
    // Bounds parallel index builds (e.g. the client-side IVFFlat rebuild after applying packages).
    let max_parallel_maintenance_workers = pg_runtime_setting(
        "JURISEARCH_PG_MAX_PARALLEL_MAINTENANCE_WORKERS",
        "2",
        is_pg_int_literal,
    );
    runtime_conf.push_str(&format!(
        "effective_cache_size = '{effective_cache_size}'\n\
         work_mem = '{work_mem}'\n\
         maintenance_work_mem = '{maintenance_work_mem}'\n\
         temp_buffers = '{temp_buffers}'\n\
         max_parallel_workers_per_gather = '{max_parallel_workers_per_gather}'\n\
         max_parallel_workers = '{max_parallel_workers}'\n\
         max_parallel_maintenance_workers = '{max_parallel_maintenance_workers}'\n",
    ));
    // shared_buffers is the dominant RAM claim — keep its default small and tunable. The bulk-ingest
    // profile gets a slightly larger default for checkpoint efficiency during a transient load; both
    // honor the same `JURISEARCH_PG_SHARED_BUFFERS` operator override.
    let default_shared_buffers = match profile {
        PostgresRuntimeProfile::BulkIngest => "512MB",
        PostgresRuntimeProfile::Durable => "256MB",
    };
    let shared_buffers = pg_runtime_setting(
        "JURISEARCH_PG_SHARED_BUFFERS",
        default_shared_buffers,
        is_pg_mem_literal,
    );
    match profile {
        PostgresRuntimeProfile::BulkIngest => {
            runtime_conf.push_str(&format!(
                "synchronous_commit = 'off'\n\
                 wal_compression = 'on'\n\
                 max_wal_size = '8GB'\n\
                 checkpoint_timeout = '30min'\n\
                 checkpoint_completion_target = '0.9'\n\
                 shared_buffers = '{shared_buffers}'\n",
            ));
        }
        PostgresRuntimeProfile::Durable => {
            // No bulk WAL relaxation on the read-heavy search/eval profile.
            runtime_conf.push_str(&format!("shared_buffers = '{shared_buffers}'\n"));
        }
    }

    fs::write(data_dir.join("jurisearch.conf"), runtime_conf).map_err(StorageError::Io)
}

fn apply_runtime_profile(
    pg_config: &PgConfig,
    port: u16,
    database: &str,
    profile: PostgresRuntimeProfile,
) -> Result<(), StorageError> {
    match profile {
        PostgresRuntimeProfile::BulkIngest => {
            psql(
                pg_config,
                port,
                BOOTSTRAP_DATABASE,
                &format!(
                    "ALTER DATABASE {} SET synchronous_commit = 'off';",
                    sql_identifier(database)
                ),
            )?;
        }
        PostgresRuntimeProfile::Durable => {
            psql(
                pg_config,
                port,
                BOOTSTRAP_DATABASE,
                &format!(
                    "ALTER DATABASE {} RESET synchronous_commit;",
                    sql_identifier(database)
                ),
            )?;
            // Defensive cleanup for live/manual tuning applied through
            // postgresql.auto.conf; the bulk profile itself uses jurisearch.conf.
            psql(
                pg_config,
                port,
                BOOTSTRAP_DATABASE,
                "ALTER SYSTEM RESET max_wal_size;",
            )?;
            psql(
                pg_config,
                port,
                BOOTSTRAP_DATABASE,
                "SELECT pg_reload_conf();",
            )?;
        }
    }
    Ok(())
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

/// Quote a SQL identifier (schema/table/column) by doubling embedded quotes — so a name derived from a
/// corpus/generation can never break out of the statement. Public so the consumer service can build
/// schema-qualified `COPY` statements (plan P3).
#[must_use]
pub fn sql_identifier(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

/// Quote a SQL string literal by doubling embedded single quotes. Public so the producer builder can
/// compose scope-predicate SQL (plan P4).
#[must_use]
pub fn sql_string_literal(value: &str) -> String {
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
    #[error(
        "database schema version {database_version} is newer than this binary supports ({binary_version})"
    )]
    SchemaVersionAhead {
        database_version: i32,
        binary_version: i32,
    },
    #[error("invalid migration plan: {message}")]
    MigrationPlan { message: String },
    #[error("canonical projection failed: {message}")]
    Projection { message: String },
    #[error("dense rebuild failed: {message}")]
    DenseRebuild { message: String },
    #[error("retrieval failed: {message}")]
    Retrieval { message: String },
    #[error("ingest accounting failed: {message}")]
    IngestAccounting { message: String },
    #[error("change-log outbox failed: {message}")]
    Outbox { message: String },
    #[error("generation topology failed: {message}")]
    Generations { message: String },
    #[error("package catalog conflict: {message}")]
    PackageCatalog { message: String },
    #[error("json serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("postgres client error: {0}")]
    PostgresClient(postgres::Error),
    #[error(transparent)]
    Io(io::Error),
}

#[cfg(test)]
mod tests {
    use super::{
        DataDirLock, ManagedPostgres, PgConfig, StorageError, data_dir_lock_key, is_pg_int_literal,
        is_pg_mem_literal, resolve_pg_setting, sql_identifier, sql_string_literal, version_key,
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
    fn pg_setting_literals_reject_unsafe_overrides() {
        // Well-formed memory literals within [1 MiB, ~2 TiB] — including fractional and `B`-unit forms
        // Postgres accepts, and a value just under the MAX_KILOBYTES ceiling — are valid.
        for ok in [
            "1MB", "256MB", "2GB", "1.5GB", "1TB", "8GB", "64GB", "1048576B", "2047GB",
        ] {
            assert!(is_pg_mem_literal(ok), "{ok} should be a valid mem literal");
        }
        for bad in [
            "256",      // unit required
            "256 MB",   // no embedded space
            "256mb",    // case-sensitive unit
            "25%",      // not a size
            "256MB; x", // config-injection attempt
            "'256MB'",  // already-quoted
            "",         // empty
            "GB",       // no digits
            "-1GB",     // no sign
            "1e3MB",    // no exponent form
            "0MB",      // below the floor → Postgres refuses to start
            "512kB",    // 0.5 MiB, below the uniform 1 MiB floor
            "1.GB",     // malformed decimal
            ".5GB",     // malformed decimal
            "2048GB",   // == 2 TiB, just over the MAX_KILOBYTES ceiling → startup FATAL
            "2TB",      // over the ceiling
            "8192GB",   // over the ceiling
            "16TB",     // over the ceiling
        ] {
            assert!(!is_pg_mem_literal(bad), "{bad:?} must be rejected");
        }

        // Worker counts: bare integers in [0, 1024]. Signs, overflow, and out-of-range are rejected so
        // a hostile value can never break startup.
        for ok in ["0", "8", "1024"] {
            assert!(is_pg_int_literal(ok), "{ok} should be a valid int literal");
        }
        for bad in [
            "",
            "-1",
            "+1", // Rust's parse would accept the leading `+`; the digit check rejects it
            "2 ",
            "4workers",
            "0x4",
            "2;DROP",
            "1025",                                    // above the GUC cap
            "999999999999999999999999999999999999999", // overflows u32
        ] {
            assert!(!is_pg_int_literal(bad), "{bad:?} must be rejected");
        }
    }

    #[test]
    fn resolve_pg_setting_prefers_valid_override_else_default() {
        // No override → default.
        assert_eq!(
            resolve_pg_setting(None, "256MB", is_pg_mem_literal),
            "256MB"
        );
        // A valid override (trimmed) wins over the default.
        assert_eq!(
            resolve_pg_setting(Some("  4GB "), "256MB", is_pg_mem_literal),
            "4GB"
        );
        // Malformed, sub-floor, and injection-shaped overrides all fall back to the conservative
        // default rather than reaching the conf file.
        for bad in ["rm -rf", "0MB", "256MB; evil = 1"] {
            assert_eq!(
                resolve_pg_setting(Some(bad), "256MB", is_pg_mem_literal),
                "256MB",
                "{bad:?} should fall back to the default"
            );
        }
        // An overflowing worker count also falls back instead of breaking startup.
        assert_eq!(
            resolve_pg_setting(Some("999999999999"), "4", is_pg_int_literal),
            "4"
        );
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
