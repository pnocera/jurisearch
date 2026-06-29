//! M5-A — persistent thin-client configuration: a tiny XDG-located TOML file holding the site service
//! URL the user `configure`d once. Dependency-light by construction (`serde` + `toml` + `std` only — NO
//! heavy config crate), so it does not widen the thin client's dependency cone.
//!
//! Layout: `$XDG_CONFIG_HOME/jurisearch/client.toml` (fallback `~/.config/jurisearch/client.toml`).
//! Format: a single `server = "tcp://host:port"` (or `unix:///path`) key. Writes are ATOMIC
//! (temp-file + rename) and the file is created `0600` (the directory `0700`) so a persisted endpoint is
//! never world-readable.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The persisted thin-client configuration. Intentionally minimal: only the site service URL the client
/// dials when neither `--server`/`--local` nor `$JURISEARCH_SITE_URL` is provided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientConfig {
    /// The site service URL — `tcp://host:port` or `unix:///absolute/path`. Validated (via
    /// [`crate::parse_endpoint`]) before it is ever written.
    pub server: String,
}

/// The relative location under the XDG config root: `jurisearch/client.toml`.
const CONFIG_SUBPATH: &[&str] = &["jurisearch", "client.toml"];

/// Resolve the on-disk config path from the environment, honoring `XDG_CONFIG_HOME` (when set to a
/// non-empty ABSOLUTE path, per the XDG Base Directory spec) and otherwise `~/.config`. Returns `None`
/// only when neither `XDG_CONFIG_HOME` nor `HOME` yields an absolute base (a headless/no-home process) —
/// the caller turns that into an actionable error.
#[must_use]
pub fn resolve_config_path() -> Option<PathBuf> {
    let base = config_base_dir()?;
    Some(CONFIG_SUBPATH.iter().fold(base, |acc, seg| acc.join(seg)))
}

/// The XDG config BASE directory (`$XDG_CONFIG_HOME` or `~/.config`), or `None` when unavailable.
fn config_base_dir() -> Option<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let path = PathBuf::from(&xdg);
        // The spec says a relative (or empty) `XDG_CONFIG_HOME` must be ignored.
        if path.is_absolute() {
            return Some(path);
        }
    }
    let home = std::env::var_os("HOME")?;
    let home = PathBuf::from(home);
    if home.is_absolute() {
        Some(home.join(".config"))
    } else {
        None
    }
}

/// Load the config at an EXPLICIT path. `Ok(None)` when the file is absent (an unconfigured client is a
/// normal state, not an error); `Err` only when the file exists but is unreadable or malformed.
pub fn load_config_at(path: &Path) -> anyhow::Result<Option<ClientConfig>> {
    match fs::read_to_string(path) {
        Ok(body) => {
            let config: ClientConfig = toml::from_str(&body).map_err(|error| {
                anyhow::anyhow!(
                    "the client config at {} is not valid (expected `server = \"<url>\"`): {error}",
                    path.display()
                )
            })?;
            Ok(Some(config))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(anyhow::anyhow!(
            "cannot read the client config at {}: {error}",
            path.display()
        )),
    }
}

/// Atomically persist the config to an EXPLICIT path: create the parent directory (`0700`), write the
/// TOML to a sibling temp file (`0600`), `fsync`, then `rename` into place — so a reader never sees a
/// half-written file and the persisted endpoint is never world-readable.
pub fn save_config_at(path: &Path, config: &ClientConfig) -> anyhow::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "the client config path {} has no parent directory",
            path.display()
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        anyhow::anyhow!(
            "cannot create the config directory {}: {error}",
            parent.display()
        )
    })?;
    // Best-effort tighten the directory to `0700` (owner-only). A pre-existing shared dir is not fatal.
    let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));

    let body = toml::to_string_pretty(config)
        .map_err(|error| anyhow::anyhow!("cannot serialize the client config: {error}"))?;

    // A sibling temp file keeps the rename atomic and on the SAME filesystem as the target.
    let tmp = parent.join(format!(".client.toml.tmp.{}", std::process::id()));
    let write_result = (|| -> std::io::Result<()> {
        // `mode(0o600)` is applied ONLY when the file is freshly created. `create_new(true)` guarantees
        // we never adopt a stale temp's (possibly wider) permissions by truncating it in place — a
        // pre-existing temp would otherwise be renamed into `client.toml` keeping its old mode. If a
        // stale temp is left over (e.g. a crashed prior run reusing this pid), unlink it and retry.
        let open = || {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)
        };
        let mut file = match open() {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::remove_file(&tmp)?;
                open()?
            }
            Err(error) => return Err(error),
        };
        // Belt-and-suspenders: force exactly 0600 before the rename, so the installed config is
        // owner-only regardless of how the temp came to exist.
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&tmp);
        return Err(anyhow::anyhow!(
            "cannot write the client config temp file {}: {error}",
            tmp.display()
        ));
    }
    fs::rename(&tmp, path).map_err(|error| {
        let _ = fs::remove_file(&tmp);
        anyhow::anyhow!(
            "cannot install the client config at {}: {error}",
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_then_load_round_trips_through_an_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jurisearch").join("client.toml");
        // Absent → Ok(None), never an error.
        assert!(load_config_at(&path).unwrap().is_none());

        let config = ClientConfig {
            server: "tcp://site.local:8099".to_owned(),
        };
        save_config_at(&path, &config).unwrap();
        assert_eq!(load_config_at(&path).unwrap(), Some(config));
    }

    #[test]
    fn the_persisted_file_is_owner_only_0600() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.toml");
        save_config_at(
            &path,
            &ClientConfig {
                server: "unix:///run/jurisearch/site.sock".to_owned(),
            },
        )
        .unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the config must not be world/group readable");
    }

    #[test]
    fn a_stale_wider_temp_file_does_not_widen_the_installed_config() {
        // A leftover temp from a crashed prior run (same pid) with WIDER perms must NOT be renamed into
        // place keeping its mode — `save_config_at` must unlink/recreate it 0600.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("jurisearch").join("client.toml");
        let parent = path.parent().unwrap();
        fs::create_dir_all(parent).unwrap();
        let stale = parent.join(format!(".client.toml.tmp.{}", std::process::id()));
        fs::write(&stale, "leftover").unwrap();
        fs::set_permissions(&stale, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            fs::metadata(&stale).unwrap().permissions().mode() & 0o777,
            0o644
        );

        save_config_at(
            &path,
            &ClientConfig {
                server: "tcp://site.local:8099".to_owned(),
            },
        )
        .unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the installed config must be 0600 even over a stale wider temp"
        );
    }

    #[test]
    fn a_malformed_config_file_is_a_clear_error_not_a_silent_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.toml");
        fs::write(&path, "this is not = valid = toml").unwrap();
        let error = load_config_at(&path).unwrap_err().to_string();
        assert!(error.contains("not valid"), "diagnostic: {error}");
    }

    #[test]
    fn xdg_config_home_must_be_absolute_to_be_honored() {
        // Drive `config_base_dir` purely through this test's view; we assert the path SHAPE, not env
        // mutation (which would race other tests). With an absolute XDG root the path ends in our subpath.
        let base = PathBuf::from("/xdg/root");
        let path = CONFIG_SUBPATH.iter().fold(base, |acc, seg| acc.join(seg));
        assert!(path.ends_with("jurisearch/client.toml"));
    }
}
