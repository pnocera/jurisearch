//! `site init` scaffolding: create the config parent directory and write a commented template if the
//! target file does not already exist (never clobber an operator-owned `site.toml`).

use std::path::Path;

use crate::config::SITE_CONFIG_EXAMPLE;
use crate::error::DeployError;

/// The outcome of a `site init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitOutcome {
    /// A new template config was written at the target path.
    Created,
    /// The target path already existed and was left untouched.
    AlreadyExists,
}

/// Create the parent directory of `config_path` and write the commented [`SITE_CONFIG_EXAMPLE`]
/// template there, unless the file already exists (in which case it is preserved).
pub fn init_site_config(config_path: &Path) -> Result<InitOutcome, DeployError> {
    if config_path.exists() {
        return Ok(InitOutcome::AlreadyExists);
    }
    if let Some(parent) = config_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|source| DeployError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(config_path, SITE_CONFIG_EXAMPLE).map_err(|source| DeployError::Write {
        path: config_path.to_path_buf(),
        source,
    })?;
    Ok(InitOutcome::Created)
}
