//! External-PostgreSQL provisioning (work/10 M1-B seam S3).
//!
//! One typed entrypoint — [`provision_external_db`] — that takes an operator-run PostgreSQL from "blank
//! server" to "the producer can write packages into it", with NO [`ManagedPostgres`](crate::runtime::ManagedPostgres):
//!
//! 1. create the target database if missing (admin/maintenance connection);
//! 2. run the canonical migrations over a connection (seam S2 [`run_migrations_on`], which also installs
//!    the required extensions or surfaces the exact DBA SQL when superuser is needed);
//! 3. provision the least-privilege owner/writer/read roles + grants for the EXTERNAL PRODUCER
//!    ([`provision_producer_roles`] — the writer is the corpus build principal, so it gets DML across the
//!    FULL `public` working schema, not the site applier's narrow enumerated surface); and
//! 4. verify an **activation-visibility postcondition**: the schema/tables/extensions are present, the
//!    writer role CAN write, and the read role CANNOT — least-privilege proven, not assumed.
//!
//! It is IDEMPOTENT: re-running converges (database already present, migrations already applied, roles
//! re-converged) with no error. It operates PURELY through [`ConnectionConfig`]/[`DbClientSource`] client
//! sources, so the external path can never silently fall back to a managed server. Secrets follow the
//! established discipline — [`ConnectionConfig`]/[`RoleSpec`] redact passwords in `Debug`, and this module
//! never logs them.

use thiserror::Error;

use crate::backend::{ConnectionConfig, RoleSpec, provision_producer_roles};
use crate::migrations::{MigrationReport, REQUIRED_EXTENSIONS, run_migrations_on};
use crate::runtime::{StorageError, sql_identifier};

/// A unique, self-identifying probe key the writer/read postcondition writes inside a ROLLED-BACK
/// transaction, so the privilege check leaves no residue in `index_manifest`.
const PROVISION_PROBE_KEY: &str = "__jurisearch_provision_probe__";
const PROBE_APPLICATION_NAME: &str = "jurisearch-provision-probe";

/// Typed provisioning request (work/10 §2 S3). All connection secrets live in the redacting
/// [`ConnectionConfig`]/[`RoleSpec`] types.
#[derive(Debug, Clone)]
pub struct ProvisionConfig {
    /// The admin/maintenance connection used to `CREATE DATABASE`, `CREATE ROLE`, and (if needed) create
    /// extensions. Its `dbname` is the MAINTENANCE database to connect to for the `CREATE DATABASE` step
    /// (e.g. `postgres`) — NOT the target. The role must be able to create databases and roles, and to
    /// create the required extensions (superuser) unless they are pre-installed by a DBA.
    pub admin: ConnectionConfig,
    /// The database to provision (created if missing). The schema, roles and grants land here.
    pub target_db: String,
    /// The least-privilege owner/writer/read role names + (optional) passwords to provision.
    pub roles: RoleSpec,
}

/// Typed provisioning outcome.
#[derive(Debug, Clone)]
pub struct ProvisionReport {
    /// `true` if this call created the database, `false` if it already existed (idempotent re-run).
    pub database_created: bool,
    /// The schema version present after migration (`CURRENT_SCHEMA_VERSION`).
    pub schema_version: i32,
    /// Every schema version present after the migration run.
    pub applied_migrations: Vec<i32>,
    /// The role names provisioned, in `[owner, writer, read]` order.
    pub roles_provisioned: Vec<String>,
    /// The required extensions confirmed present after provisioning.
    pub extensions_present: Vec<String>,
    /// Postcondition: the writer role could write (a rolled-back probe INSERT succeeded).
    pub writer_can_write: bool,
    /// Postcondition: whether the read role could write (MUST be `false`; provisioning fails otherwise).
    pub read_role_can_write: bool,
}

/// Provisioning errors. Storage/migration failures (including the actionable
/// [`StorageError::ExtensionPrivilege`]) surface through [`ProvisionError::Storage`]; a violated
/// least-privilege/visibility postcondition surfaces as [`ProvisionError::Postcondition`].
#[derive(Debug, Error)]
pub enum ProvisionError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error("provisioning postcondition failed: {0}")]
    Postcondition(String),
}

/// Provision (or converge) an external PostgreSQL: create the database, migrate, provision roles, and
/// verify the least-privilege/visibility postcondition. See the module docs for the full contract.
///
/// # Errors
/// [`ProvisionError::Storage`] for any DB/migration error (incl. [`StorageError::ExtensionPrivilege`]
/// when a required extension needs a superuser); [`ProvisionError::Postcondition`] if the writer cannot
/// write or the read role CAN write after provisioning.
pub fn provision_external_db(cfg: &ProvisionConfig) -> Result<ProvisionReport, ProvisionError> {
    // 1. Create the target database if missing (maintenance connection; CREATE DATABASE cannot run in a
    //    transaction and is not parameterizable, so the name is identifier-quoted).
    let mut maintenance = cfg.admin.connect()?;
    let database_created = ensure_target_database(&mut maintenance, &cfg.target_db)?;
    drop(maintenance);

    // 2. Migrate the target over a connection (seam S2) — installs/validates required extensions.
    let target_admin = ConnectionConfig {
        dbname: cfg.target_db.clone(),
        ..cfg.admin.clone()
    };
    let mut admin = target_admin.connect()?;
    let migration: MigrationReport = run_migrations_on(&mut admin)?;

    // 3. Least-privilege roles + grants for the EXTERNAL PRODUCER (idempotent/convergent). The producer
    //    profile gives the writer DML across the FULL `public` working schema (not the site's narrow
    //    enumerated surface), because S3 provisions the box that BUILDS the corpus — see `RoleProfile`.
    provision_producer_roles(&mut admin, &cfg.roles, &cfg.target_db)?;

    // 4. Activation-visibility / least-privilege postcondition.
    let extensions_present = confirm_extensions_present(&mut admin)?;
    confirm_schema_present(&mut admin)?;
    let writer_can_write = probe_role_can_write(&target_admin, &cfg.roles.writer_role, cfg)?;
    if !writer_can_write {
        return Err(ProvisionError::Postcondition(format!(
            "writer role `{}` cannot write to the provisioned database",
            cfg.roles.writer_role
        )));
    }
    let read_role_can_write = probe_role_can_write(&target_admin, &cfg.roles.read_role, cfg)?;
    if read_role_can_write {
        return Err(ProvisionError::Postcondition(format!(
            "read role `{}` CAN write — least privilege violated",
            cfg.roles.read_role
        )));
    }

    Ok(ProvisionReport {
        database_created,
        schema_version: migration.current_version,
        applied_migrations: migration.applied,
        roles_provisioned: vec![
            cfg.roles.owner_role.clone(),
            cfg.roles.writer_role.clone(),
            cfg.roles.read_role.clone(),
        ],
        extensions_present,
        writer_can_write,
        read_role_can_write,
    })
}

/// Create `target_db` if it does not already exist. Returns `true` if it was created.
fn ensure_target_database(
    admin: &mut postgres::Client,
    target_db: &str,
) -> Result<bool, StorageError> {
    let exists: bool = admin
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1);",
            &[&target_db],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    if exists {
        return Ok(false);
    }
    admin
        .batch_execute(&format!("CREATE DATABASE {};", sql_identifier(target_db)))
        .map_err(StorageError::PostgresClient)?;
    Ok(true)
}

/// Confirm every required extension is present in the target database (postcondition, not creation).
fn confirm_extensions_present(client: &mut postgres::Client) -> Result<Vec<String>, StorageError> {
    let mut present = Vec::with_capacity(REQUIRED_EXTENSIONS.len());
    for extension in REQUIRED_EXTENSIONS {
        let exists: bool = client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = $1);",
                &[extension],
            )
            .map_err(StorageError::PostgresClient)?
            .get(0);
        if !exists {
            return Err(StorageError::Generations {
                message: format!("required extension `{extension}` is absent after provisioning"),
            });
        }
        present.push((*extension).to_owned());
    }
    Ok(present)
}

/// Confirm the migrated schema is materialized: a representative `public` table, the control table, and
/// a stable server view all resolve (the activation-visibility surface the read/writer roles depend on).
fn confirm_schema_present(client: &mut postgres::Client) -> Result<(), StorageError> {
    for relation in [
        "public.documents",
        "public.schema_migrations",
        "jurisearch_control.corpus_state",
        "jurisearch_server.documents",
    ] {
        let exists: bool = client
            .query_one("SELECT to_regclass($1) IS NOT NULL;", &[&relation])
            .map_err(StorageError::PostgresClient)?
            .get(0);
        if !exists {
            return Err(StorageError::Generations {
                message: format!("expected relation `{relation}` is absent after provisioning"),
            });
        }
    }
    Ok(())
}

/// Connect as `role` and probe whether it can write, by attempting REPRESENTATIVE producer writes inside
/// a transaction that is ALWAYS rolled back (no residue). The probe exercises more than an
/// `index_manifest` upsert: it writes the public ingest-accounting tables the producer hits at its first
/// run (`INSERT INTO public.ingest_run` and an `INSERT INTO public.ingest_member` that allocates from the
/// `bigserial` `member_id` sequence), AND — the decisive producer check — a REPLICATED public WORKING
/// table: `INSERT INTO public.official_api_responses`, which draws from its `bigserial` `response_id`
/// public sequence. So a producer writer that lacks DML on a replicated working table or USAGE on its
/// public sequence is caught HERE (writer probe), not at the producer's first projection; and the read
/// role is confirmed denied these same writes. Returns `true` if every write was permitted, `false` if
/// any was denied for insufficient privilege (SQLSTATE 42501). Any other (non-privilege) error surfaces
/// as [`StorageError`] so a real fault is not mistaken for a denial.
fn probe_role_can_write(
    target_admin: &ConnectionConfig,
    role: &str,
    cfg: &ProvisionConfig,
) -> Result<bool, StorageError> {
    let password = if role == cfg.roles.writer_role {
        cfg.roles.writer_password.clone()
    } else if role == cfg.roles.read_role {
        cfg.roles.read_password.clone()
    } else {
        None
    };
    let role_cfg = ConnectionConfig {
        user: role.to_owned(),
        password,
        application_name: PROBE_APPLICATION_NAME.to_owned(),
        ..target_admin.clone()
    };
    let mut client = role_cfg.connect()?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    let outcome = run_write_probe(&mut tx);
    // Roll back regardless, so neither a successful writer probe nor a denied read probe persists.
    let _ = tx.rollback();
    match outcome {
        Ok(()) => Ok(true),
        Err(error)
            if error.as_db_error().is_some_and(|db| {
                db.code() == &postgres::error::SqlState::INSUFFICIENT_PRIVILEGE
            }) =>
        {
            Ok(false)
        }
        Err(error) => Err(StorageError::PostgresClient(error)),
    }
}

/// Run the representative producer write probe inside a (caller-rolled-back) transaction. Short-circuits
/// on the first denial: an `index_manifest` upsert, then the ingest-accounting writes (`ingest_run`, then
/// `ingest_member` which draws from a public `bigserial` sequence), then a REPLICATED public WORKING-table
/// write (`official_api_responses`, drawing from its `bigserial` `response_id` public sequence) — the
/// statement that proves the PRODUCER profile actually granted the full public working schema + its
/// sequences (a site-style enumerated grant would deny it). Each statement is a VALID insert against the
/// real schema — every NOT NULL column is supplied (incl. the migration-18 NOT NULL `corpus` on
/// `official_api_responses`, set to `'core'`; `corpus` carries no FK so the value is unconstrained on a
/// fresh DB), every CHECK is satisfied (`provider`/`http_method`/`outcome`/`status` use allowed values),
/// and the only FK (`ingest_member.run_id` → `ingest_run.run_id`) is satisfied by the `ingest_run` row
/// inserted earlier in the SAME transaction. `official_api_responses` deliberately has NO FK. So a failure
/// is a genuine privilege denial (42501) — never a NOT NULL / CHECK / FK error masquerading as one.
fn run_write_probe(tx: &mut postgres::Transaction<'_>) -> Result<(), postgres::Error> {
    tx.execute(
        "INSERT INTO public.index_manifest (key, value) VALUES ($1, '{}'::jsonb) \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value;",
        &[&PROVISION_PROBE_KEY],
    )?;
    tx.execute(
        "INSERT INTO public.ingest_run \
           (run_id, source, status, parser_version, schema_version, code_version) \
         VALUES ($1, 'provision-probe', 'running', 'probe', 'probe', 'probe');",
        &[&PROVISION_PROBE_KEY],
    )?;
    tx.execute(
        "INSERT INTO public.ingest_member \
           (run_id, archive_name, member_path, source, status, parser_version, schema_version, \
            code_version, source_payload_hash) \
         VALUES ($1, 'provision-probe', 'provision-probe', 'provision-probe', 'discovered', \
            'probe', 'probe', 'probe', 'probe');",
        &[&PROVISION_PROBE_KEY],
    )?;
    // The decisive producer-surface check: a replicated public WORKING table whose `bigserial`
    // `response_id` draws from a public sequence. Only the PRODUCER grant profile (full public DML +
    // `USAGE` on all public sequences) permits this; the read role and a site-style writer are denied.
    tx.execute(
        "INSERT INTO public.official_api_responses \
           (provider, endpoint, http_method, request_fingerprint, outcome, response_body_sha256, \
            corpus, run_id) \
         VALUES ('local', 'provision-probe', 'LOCAL', $1, 'ok', 'probe', 'core', $1);",
        &[&PROVISION_PROBE_KEY],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provision_config_debug_redacts_secrets() {
        let cfg = ProvisionConfig {
            admin: ConnectionConfig {
                host: "127.0.0.1".to_owned(),
                port: 5432,
                dbname: "postgres".to_owned(),
                user: "postgres".to_owned(),
                password: Some("admin-secret".to_owned()),
                application_name: "jurisearch-provision".to_owned(),
            },
            target_db: "jurisearch".to_owned(),
            roles: RoleSpec {
                read_password: Some("read-secret".to_owned()),
                writer_password: Some("write-secret".to_owned()),
                ..RoleSpec::default()
            },
        };
        let rendered = format!("{cfg:?}");
        for secret in ["admin-secret", "read-secret", "write-secret"] {
            assert!(!rendered.contains(secret), "secret leaked: {rendered}");
        }
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }
}
