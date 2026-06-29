//! Shared config primitives: secret redaction + secret-file permission helpers.
//!
//! These are deliberately generic (no `SiteConfig` knowledge) so the later producer config parser
//! (M2-B) reuses the exact same secret-handling code instead of inventing a parallel path.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// The fixed placeholder substituted for any secret value in diagnostics, logs, or `Debug` output.
pub const REDACTED: &str = "[redacted]";

/// Redact a secret for display. Never echoes the secret bytes; emits a stable placeholder.
#[must_use]
pub fn redact(_secret: &str) -> &'static str {
    REDACTED
}

/// A string that holds a secret (password, token, API key) and never prints its value through
/// `Debug` or `Display`. Use `expose()` only at the actual boundary that needs the bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Borrow the raw secret. Call this only where the cleartext is genuinely required.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "SecretString({REDACTED})")
    }
}

impl std::fmt::Display for SecretString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(REDACTED)
    }
}

impl From<String> for SecretString {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// The owner-only file mode for generated secret/env files.
pub const SECRET_FILE_MODE: u32 = 0o600;

/// The world-readable bits (group-read + other-read). A secret file must have none of these set.
pub const WORLD_AND_GROUP_READABLE_BITS: u32 = 0o077;

/// Write `contents` to `path`, creating/truncating it with `0600` permissions. The mode is set on
/// `open` (via `O_CREAT` mode) AND re-applied after write so a pre-existing looser file is tightened.
pub fn write_secret_file(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(SECRET_FILE_MODE)
        .open(path)?;
    file.write_all(contents)?;
    file.flush()?;
    // Re-apply the mode unconditionally: `O_CREAT` mode is masked by umask and is ignored entirely
    // when the file already existed, so an explicit `set_permissions` is what actually guarantees 0600.
    fs::set_permissions(path, fs::Permissions::from_mode(SECRET_FILE_MODE))?;
    Ok(())
}

/// Write a non-secret generated file (e.g. a systemd unit) with the given mode.
pub fn write_file_with_mode(path: &Path, contents: &[u8], mode: u32) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(path)?;
    file.write_all(contents)?;
    file.flush()?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

/// The permission mode bits (`& 0o7777`) of an existing file.
pub fn file_mode(path: &Path) -> std::io::Result<u32> {
    Ok(fs::metadata(path)?.permissions().mode() & 0o7777)
}

/// True when `path` grants any group/other read (or any group/other access at all). Used to reject a
/// world-readable password file before it is trusted.
pub fn is_world_or_group_accessible(path: &Path) -> std::io::Result<bool> {
    Ok(file_mode(path)? & WORLD_AND_GROUP_READABLE_BITS != 0)
}
