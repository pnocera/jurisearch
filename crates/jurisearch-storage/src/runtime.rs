use std::{
    fs, io,
    net::TcpListener,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use tempfile::TempDir;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
        let home = std::env::var("HOME").map_err(|_| StorageError::MissingHome)?;
        let default = PathBuf::from(home).join(".pgrx/18.4/pgrx-install/bin/pg_config");
        Self::from_path(default)
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

#[derive(Debug)]
pub struct ManagedPostgres {
    _tmp: TempDir,
    pub pg_config: PgConfig,
    pub data_dir: PathBuf,
    pub socket_dir: PathBuf,
    pub log_path: PathBuf,
    pub port: u16,
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
            _tmp: tmp,
            pg_config,
            data_dir,
            socket_dir,
            log_path,
            port,
        })
    }

    pub fn execute_sql(&self, sql: &str) -> Result<String, StorageError> {
        let port = self.port.to_string();
        let output = Command::new(self.pg_config.bindir.join("psql"))
            .args([
                "-h",
                "127.0.0.1",
                "-p",
                &port,
                "-U",
                "postgres",
                "-d",
                "postgres",
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

impl Drop for ManagedPostgres {
    fn drop(&mut self) {
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
    #[error(transparent)]
    Io(io::Error),
}
