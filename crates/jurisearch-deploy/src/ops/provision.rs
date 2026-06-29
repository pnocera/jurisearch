//! Site database provisioning (plan `01` Phase 3, consumed by `site install`).
//!
//! This is OPERATOR ORCHESTRATION over `jurisearch-storage` primitives — it does not reimplement any
//! schema/role logic. It composes, with the SITE role profile (the narrow read/write confidentiality
//! split, NOT the producer's full-`public` build authority):
//!
//! 1. `CREATE DATABASE` if missing (admin maintenance connection);
//! 2. `jurisearch_storage::migrations::run_migrations_on` (seam S2 — installs/validates the required
//!    extensions or surfaces the exact DBA SQL);
//! 3. `jurisearch_storage::backend::provision_roles` (the SITE `RoleProfile`).
//!
//! It is idempotent: a re-run converges (DB present, migrations applied, roles re-converged).
//!
//! The external-PG `provision_external_db` is deliberately NOT used here: it provisions the PRODUCER role
//! profile (full `public` DML), which is wrong for a customer site whose read/write split is a
//! confidentiality boundary.

use jurisearch_storage::backend::{ConnectionConfig, RoleSpec, provision_roles};
use jurisearch_storage::migrations::run_migrations_on;
use jurisearch_storage::runtime::sql_identifier;

use crate::config::SiteConfig;
use crate::error::DeployError;

use super::connection::{admin_maintenance_config, provision_config};

/// The outcome of a site provisioning run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvisionSummary {
    pub database_created: bool,
    pub schema_version: i32,
    pub roles: Vec<String>,
}

/// Provision (or converge) the site database with the SITE role profile. Live: opens admin connections.
///
/// # Errors
/// [`DeployError`] when the admin connection fails or any provisioning step errors (the message carries
/// the exact next action, e.g. an extension that needs a DBA `CREATE EXTENSION`).
pub fn provision_site_db(config: &SiteConfig) -> Result<ProvisionSummary, DeployError> {
    let admin_maintenance = admin_maintenance_config(config)?;
    let provision = provision_config(config)?;

    // 1. Create the target database if missing (maintenance connection).
    let mut maintenance = admin_maintenance
        .connect()
        .map_err(|error| connect_error("provision.admin.connect", &admin_maintenance, error))?;
    let database_created = ensure_database(&mut maintenance, &provision.target_db)?;
    drop(maintenance);

    // 2. Migrate the target as the admin/bootstrap identity (NOT the read or writer role).
    let target_admin = ConnectionConfig {
        dbname: provision.target_db.clone(),
        ..admin_maintenance.clone()
    };
    let mut admin = target_admin
        .connect()
        .map_err(|error| connect_error("provision.target.connect", &target_admin, error))?;
    run_migrations_on(&mut admin).map_err(|error| storage_error("provision.migrate", error))?;

    // 3. Roles + grants for the SITE profile (idempotent/convergent).
    provision_roles(&mut admin, &provision.roles, &provision.target_db)
        .map_err(|error| storage_error("provision.roles", error))?;

    Ok(ProvisionSummary {
        database_created,
        schema_version: jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
        roles: role_names(&provision.roles),
    })
}

fn ensure_database(admin: &mut postgres::Client, target_db: &str) -> Result<bool, DeployError> {
    let exists: bool = admin
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1);",
            &[&target_db],
        )
        .map_err(|error| postgres_error("provision.db.exists", error))?
        .get(0);
    if exists {
        return Ok(false);
    }
    admin
        .batch_execute(&format!("CREATE DATABASE {};", sql_identifier(target_db)))
        .map_err(|error| postgres_error("provision.db.create", error))?;
    Ok(true)
}

fn role_names(roles: &RoleSpec) -> Vec<String> {
    vec![
        roles.owner_role.clone(),
        roles.writer_role.clone(),
        roles.read_role.clone(),
    ]
}

fn connect_error(
    code: &'static str,
    config: &ConnectionConfig,
    error: jurisearch_storage::runtime::StorageError,
) -> DeployError {
    let mut errors = crate::error::ValidationErrors::default();
    errors.push(
        code,
        format!(
            "could not open the admin/bootstrap connection to {}:{} db `{}` as `{}`: {error}",
            config.host, config.port, config.dbname, config.user
        ),
        "check [database].admin_user/admin_database + the admin password file / pg_hba",
    );
    DeployError::Validation(errors)
}

fn storage_error(
    code: &'static str,
    error: jurisearch_storage::runtime::StorageError,
) -> DeployError {
    let mut errors = crate::error::ValidationErrors::default();
    errors.push(
        code,
        error.to_string(),
        "follow the exact action in the error (a DBA step may be required)",
    );
    DeployError::Validation(errors)
}

fn postgres_error(code: &'static str, error: postgres::Error) -> DeployError {
    let mut errors = crate::error::ValidationErrors::default();
    errors.push(
        code,
        error.to_string(),
        "check admin privileges (CREATEDB) + connectivity",
    );
    DeployError::Validation(errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SITE_CONFIG_EXAMPLE;

    fn example() -> SiteConfig {
        SiteConfig::parse_str(SITE_CONFIG_EXAMPLE, std::path::Path::new("site.toml")).unwrap()
    }

    #[test]
    fn provision_summary_role_order_is_owner_writer_read() {
        let config = example();
        let provision = provision_config(&config).unwrap();
        let names = role_names(&provision.roles);
        assert_eq!(
            names,
            vec!["jurisearch_owner", "jurisearch_write", "jurisearch_read"]
        );
    }
}
