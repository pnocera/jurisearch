//! Build the `jurisearch-storage` connection identities from a parsed [`SiteConfig`].
//!
//! This is pure config→handle translation: it never opens a connection itself. The role topology
//! (read / writer / owner) and the admin/bootstrap identity all come straight from `[database]`, so the
//! operator commands attach to the SAME external PostgreSQL the rendered units use, with the SAME
//! least-privilege role split (`ReadHandle` for the read path, `WriterHandle` for syncd's writer path).

use std::path::Path;

use jurisearch_storage::backend::{
    ConnectionConfig, ReadHandle, RoleSpec, WriterHandle, WriterVisibility,
};
use jurisearch_storage::provision::ProvisionConfig;

use crate::config::SiteConfig;
use crate::error::DeployError;
use crate::secret;

/// Read an optional `0600` admin-password file (the password is never inline in `site.toml`). Refuses a
/// group/world-accessible file before trusting it. Returns `None` when no password file is configured
/// (peer/ident/`.pgpass`/systemd-credential paths).
pub fn read_admin_password(config: &SiteConfig) -> Result<Option<String>, DeployError> {
    let Some(path) = &config.database.admin_password_file else {
        return Ok(None);
    };
    // A configured-but-absent password file is a valid peer/ident/.pgpass/credential path → None
    // (mirrors `validate`, which only permission-checks the file WHEN it exists).
    if !path.exists() {
        return Ok(None);
    }
    if secret::is_world_or_group_accessible(path).map_err(|source| DeployError::Read {
        path: path.clone(),
        source,
    })? {
        let mut errors = crate::error::ValidationErrors::default();
        errors.push(
            "database.password_file.world_readable",
            format!(
                "database.admin_password_file `{}` is group/world-accessible",
                path.display()
            ),
            format!("chmod 0600 {}", path.display()),
        );
        return Err(DeployError::Validation(errors));
    }
    let raw = std::fs::read_to_string(path).map_err(|source| DeployError::Read {
        path: path.clone(),
        source,
    })?;
    Ok(Some(raw.trim_end_matches(['\n', '\r']).to_owned()))
}

/// The read-only identity used by the site query path + readiness lookup.
#[must_use]
pub fn read_handle(config: &SiteConfig) -> ReadHandle {
    ReadHandle::new(role_config(
        config,
        &config.database.read_user,
        "jurisearchctl-read",
    ))
}

/// The writer identity used by syncd's trust/catch-up/status path.
#[must_use]
pub fn writer_handle(config: &SiteConfig) -> WriterHandle {
    let visibility = WriterVisibility {
        read_role: config.database.read_user.clone(),
        view_owner_role: config.database.owner_role.clone(),
    };
    WriterHandle::new(
        role_config(config, &config.database.writer_user, "jurisearchctl-write"),
        visibility,
    )
}

/// The admin/bootstrap maintenance identity (connects to `admin_database` for `CREATE DATABASE`/roles).
pub fn admin_maintenance_config(config: &SiteConfig) -> Result<ConnectionConfig, DeployError> {
    Ok(ConnectionConfig {
        host: config.database.host.clone(),
        port: config.database.port,
        dbname: config.database.admin_database.clone(),
        user: config.database.admin_user.clone(),
        password: read_admin_password(config)?,
        application_name: "jurisearchctl-admin".to_owned(),
    })
}

/// The full external-DB SITE provisioning request (admin maintenance identity + target DB + role spec).
/// Passwords for the role logins are not part of the site schema (the bootstrap LAN uses peer/trust),
/// so the [`RoleSpec`] carries no role passwords here.
pub fn provision_config(config: &SiteConfig) -> Result<ProvisionConfig, DeployError> {
    Ok(ProvisionConfig {
        admin: admin_maintenance_config(config)?,
        target_db: config.database.name.clone(),
        roles: RoleSpec {
            read_role: config.database.read_user.clone(),
            writer_role: config.database.writer_user.clone(),
            owner_role: config.database.owner_role.clone(),
            read_password: None,
            writer_password: None,
        },
    })
}

fn role_config(config: &SiteConfig, user: &str, application_name: &str) -> ConnectionConfig {
    ConnectionConfig {
        host: config.database.host.clone(),
        port: config.database.port,
        dbname: config.database.name.clone(),
        user: user.to_owned(),
        password: None,
        application_name: application_name.to_owned(),
    }
}

/// Whether a path is a `0600`-or-tighter secret file (used by the password-file checks). Helper kept
/// here so the doctor + connection paths agree on the rule.
pub fn is_secret_tight(path: &Path) -> std::io::Result<bool> {
    Ok(!secret::is_world_or_group_accessible(path)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SITE_CONFIG_EXAMPLE;

    fn example() -> SiteConfig {
        SiteConfig::parse_str(SITE_CONFIG_EXAMPLE, Path::new("site.toml")).unwrap()
    }

    #[test]
    fn handles_carry_the_configured_roles_and_target_db() {
        let config = example();
        let read = read_handle(&config);
        assert_eq!(read.config().user, "jurisearch_read");
        assert_eq!(read.config().dbname, "jurisearch");
        let writer = writer_handle(&config);
        assert_eq!(writer.config().user, "jurisearch_write");
    }

    #[test]
    fn provision_config_uses_admin_database_for_maintenance_and_target_for_data() {
        let config = example();
        let provision = provision_config(&config).unwrap();
        assert_eq!(provision.admin.dbname, "postgres");
        assert_eq!(provision.admin.user, "postgres");
        assert_eq!(provision.target_db, "jurisearch");
        assert_eq!(provision.roles.writer_role, "jurisearch_write");
        assert_eq!(provision.roles.read_role, "jurisearch_read");
        assert_eq!(provision.roles.owner_role, "jurisearch_owner");
    }

    #[test]
    fn missing_password_file_yields_none_not_an_error() {
        let mut config = example();
        config.database.admin_password_file = Some(std::path::PathBuf::from("/nonexistent/secret"));
        // A configured-but-absent password file is a valid peer/ident path → None, no error.
        assert!(read_admin_password(&config).unwrap().is_none());
    }

    #[test]
    fn world_readable_password_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pw");
        std::fs::write(&path, "secret\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut config = example();
        config.database.admin_password_file = Some(path);
        assert!(read_admin_password(&config).is_err());
    }

    #[test]
    fn tight_password_file_is_read_and_trimmed() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pw");
        std::fs::write(&path, "hunter2\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let mut config = example();
        config.database.admin_password_file = Some(path);
        assert_eq!(
            read_admin_password(&config).unwrap().as_deref(),
            Some("hunter2")
        );
    }
}
