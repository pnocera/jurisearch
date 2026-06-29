//! work/10 M1-B seam S2/S3 acceptance gates — connection-based migrations + external-DB provisioning.
//!
//! The "external" PostgreSQL under test is a `ManagedPostgres::start_temp` LOOPBACK harness: we treat its
//! superuser loopback connection as the operator's ADMIN connection and provision a SEPARATE target
//! database INTO it purely through [`ConnectionConfig`]/`DbClientSource` — never by handing the
//! `ManagedPostgres` value to the provisioning API (the no-silent-fallback gate). Skips cleanly when the
//! `pgvector`/`pg_search` assets are not discoverable via `JURISEARCH_PG_CONFIG`.

mod common;

use common::discover_pg_config;
use jurisearch_storage::{
    backend::{ConnectionConfig, RoleSpec},
    migrations::{CURRENT_SCHEMA_VERSION, run_migrations_on},
    provision::{ProvisionConfig, provision_external_db},
    runtime::{ManagedPostgres, StorageError},
};

/// The admin/maintenance [`ConnectionConfig`] for the harness, treated as an EXTERNAL operator server:
/// superuser `postgres` on the loopback port, connected to the `postgres` maintenance database. No
/// `ManagedPostgres` is ever passed downstream — only this connection source.
fn admin_config(postgres: &ManagedPostgres, maintenance_db: &str) -> ConnectionConfig {
    ConnectionConfig {
        host: "127.0.0.1".to_owned(),
        port: postgres.port,
        dbname: maintenance_db.to_owned(),
        user: "postgres".to_owned(),
        password: None,
        application_name: "jurisearch-provision-test".to_owned(),
    }
}

fn target_config(admin: &ConnectionConfig, target_db: &str, user: &str) -> ConnectionConfig {
    ConnectionConfig {
        dbname: target_db.to_owned(),
        user: user.to_owned(),
        ..admin.clone()
    }
}

/// Acceptance gate: a BLANK (freshly created) PostgreSQL database can be fully provisioned — database
/// created, schema migrated, roles provisioned, least privilege proven — purely via a connection source,
/// with NO `ManagedPostgres` handed to the API (no silent fallback to a managed server).
#[test]
fn blank_external_db_is_fully_provisioned_via_connection_only() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("provision blank external db")? else {
        return Ok(());
    };
    // start_temp's database is `postgres` — our maintenance DB.
    let postgres = ManagedPostgres::start_temp(pg_config)?;
    let admin = admin_config(&postgres, &postgres.database);

    let cfg = ProvisionConfig {
        admin: admin.clone(),
        target_db: "ext_blank".to_owned(),
        roles: RoleSpec::default(),
    };

    let report = provision_external_db(&cfg).expect("blank provisioning must succeed");
    assert!(
        report.database_created,
        "the blank database must be created"
    );
    assert_eq!(report.schema_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(
        report.applied_migrations,
        (1..=CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
    );
    assert_eq!(
        report.roles_provisioned,
        vec![
            "jurisearch_owner".to_owned(),
            "jurisearch_write".to_owned(),
            "jurisearch_read".to_owned()
        ]
    );
    assert_eq!(report.extensions_present, vec!["vector", "pg_search"]);
    // Least-privilege postcondition: writer can write, read cannot.
    assert!(report.writer_can_write, "writer must be able to write");
    assert!(
        !report.read_role_can_write,
        "read role must NOT be able to write"
    );

    // Independently confirm (via the admin connection to the TARGET db) the migrated schema is real.
    let mut admin_on_target = target_config(&admin, "ext_blank", "postgres").connect()?;
    let count: i64 = admin_on_target
        .query_one("SELECT count(*) FROM public.schema_migrations;", &[])
        .map_err(StorageError::PostgresClient)?
        .get(0);
    assert_eq!(count, i64::from(CURRENT_SCHEMA_VERSION));

    Ok(())
}

/// Acceptance gate: `run_migrations_on` (the S2 connection-based applier) applied twice over a borrowed
/// client is idempotent — second run applies nothing, returns the full version set, no error.
#[test]
fn run_migrations_on_is_idempotent_over_a_borrowed_client() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("run_migrations_on idempotence")? else {
        return Ok(());
    };
    let postgres = ManagedPostgres::start_temp(pg_config)?;
    let admin = admin_config(&postgres, &postgres.database);

    // Create a blank target and migrate it twice over a connection (no ManagedPostgres).
    {
        let mut maintenance = admin.connect()?;
        maintenance
            .batch_execute("CREATE DATABASE ext_migrate_twice;")
            .map_err(StorageError::PostgresClient)?;
    }
    let target = target_config(&admin, "ext_migrate_twice", "postgres");

    let mut client = target.connect()?;
    let first = run_migrations_on(&mut client)?;
    assert_eq!(first.current_version, CURRENT_SCHEMA_VERSION);
    assert_eq!(
        first.applied,
        (1..=CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
    );

    let second = run_migrations_on(&mut client)?;
    assert_eq!(second.applied, first.applied, "second run must converge");

    let count: i64 = client
        .query_one("SELECT count(*) FROM public.schema_migrations;", &[])
        .map_err(StorageError::PostgresClient)?
        .get(0);
    assert_eq!(
        count,
        i64::from(CURRENT_SCHEMA_VERSION),
        "no duplicate migration rows"
    );

    Ok(())
}

/// Acceptance gate: `provision_external_db` applied twice is idempotent — the second run does NOT
/// recreate the database, converges migrations/roles, and the least-privilege postcondition still holds.
#[test]
fn provision_external_db_is_idempotent() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("provision idempotence")? else {
        return Ok(());
    };
    let postgres = ManagedPostgres::start_temp(pg_config)?;
    let cfg = ProvisionConfig {
        admin: admin_config(&postgres, &postgres.database),
        target_db: "ext_idem".to_owned(),
        roles: RoleSpec::default(),
    };

    let first = provision_external_db(&cfg).expect("first provisioning must succeed");
    assert!(first.database_created);

    let second = provision_external_db(&cfg).expect("re-provisioning must converge");
    assert!(
        !second.database_created,
        "second run must NOT recreate the database"
    );
    assert_eq!(second.schema_version, CURRENT_SCHEMA_VERSION);
    assert!(second.writer_can_write);
    assert!(!second.read_role_can_write);

    Ok(())
}

/// Acceptance gate: a missing required extension that the connecting role cannot create (a non-superuser
/// role + `pg_search`, which is not a trusted extension) yields the actionable
/// [`StorageError::ExtensionPrivilege`] carrying the exact DBA SQL — NOT a silent pass and not an opaque
/// failure.
#[test]
fn missing_extension_needing_superuser_yields_actionable_dba_error() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("extension privilege error")? else {
        return Ok(());
    };
    let postgres = ManagedPostgres::start_temp(pg_config)?;
    let admin = admin_config(&postgres, &postgres.database);

    // A non-superuser role owning a fresh database: it has CREATE on its own database (so a *trusted*
    // extension like `vector` may succeed) but cannot create the NON-trusted `pg_search`.
    {
        let mut superuser = admin.connect()?;
        superuser
            .batch_execute(
                "CREATE ROLE provtest_lowpriv LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE;\n\
                 CREATE DATABASE ext_lowpriv OWNER provtest_lowpriv;",
            )
            .map_err(StorageError::PostgresClient)?;
    }

    let low = target_config(&admin, "ext_lowpriv", "provtest_lowpriv");
    let mut client = low.connect()?;
    let error = run_migrations_on(&mut client)
        .expect_err("a non-superuser must not silently migrate without pg_search");
    match error {
        StorageError::ExtensionPrivilege {
            ref extension,
            ref dba_sql,
        } => {
            assert!(
                ["vector", "pg_search"].contains(&extension.as_str()),
                "unexpected extension: {extension}"
            );
            assert!(
                dba_sql.contains("CREATE EXTENSION IF NOT EXISTS"),
                "DBA SQL must be actionable: {dba_sql}"
            );
            assert!(dba_sql.contains("pg_search"), "{dba_sql}");
        }
        other => panic!("expected ExtensionPrivilege, got {other:?}"),
    }
    // The actionable message names the DBA remedy.
    assert!(error.to_string().contains("superuser"), "{error}");

    Ok(())
}

/// Acceptance gate (defense-in-depth on the no-fallback property): the provisioning surface is reachable
/// with ONLY a `ConnectionConfig` — there is no `ManagedPostgres` in `ProvisionConfig`. A read-role
/// probe is denied at the SQL layer, proving least privilege end-to-end against the connection path.
#[test]
fn read_role_write_is_denied_at_the_sql_layer() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("read role denial")? else {
        return Ok(());
    };
    let postgres = ManagedPostgres::start_temp(pg_config)?;
    let admin = admin_config(&postgres, &postgres.database);
    let cfg = ProvisionConfig {
        admin: admin.clone(),
        target_db: "ext_denial".to_owned(),
        roles: RoleSpec::default(),
    };
    let report = provision_external_db(&cfg).expect("provisioning must succeed");
    assert!(!report.read_role_can_write);

    // Connect AS the read role (a pure ConnectionConfig path) and confirm an INSERT is rejected with
    // SQLSTATE 42501 (insufficient_privilege) — not a NOT NULL / syntax error masquerading as a denial.
    let read = target_config(&admin, "ext_denial", "jurisearch_read");
    let mut read_client = read.connect()?;
    let err = read_client
        .execute(
            "INSERT INTO public.index_manifest (key, value) VALUES ('evil', '{}'::jsonb);",
            &[],
        )
        .expect_err("read role must be denied INSERT");
    let denied = err
        .as_db_error()
        .is_some_and(|db| db.code() == &postgres::error::SqlState::INSUFFICIENT_PRIVILEGE);
    assert!(denied, "expected 42501 insufficient_privilege, got {err:?}");

    // Sanity: the API genuinely never required a managed handle — provisioning above used only `admin`
    // (a ConnectionConfig). This compiles ONLY because `provision_external_db` is connection-typed.
    let _: &ConnectionConfig = &cfg.admin;
    Ok(())
}

/// Acceptance gate (M1-B BLOCKER fix): a provisioned writer can perform the PUBLIC ingest-accounting
/// writes the producer makes at its first run — `INSERT INTO public.ingest_run` AND an
/// `INSERT INTO public.ingest_member` that draws from the `bigserial` `member_id` sequence — proving both
/// the DML grant AND the sequence USAGE are in place. The read role must STILL be denied the same writes.
#[test]
fn writer_can_write_public_ingest_accounting_including_bigserial_sequence()
-> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("writer ingest-accounting write")? else {
        return Ok(());
    };
    let postgres = ManagedPostgres::start_temp(pg_config)?;
    let admin = admin_config(&postgres, &postgres.database);
    let cfg = ProvisionConfig {
        admin: admin.clone(),
        target_db: "ext_ingest".to_owned(),
        roles: RoleSpec::default(),
    };
    let report = provision_external_db(&cfg).expect("provisioning must succeed");
    // The strengthened postcondition probe already exercises an ingest-accounting write through the
    // writer role; assert it held.
    assert!(
        report.writer_can_write,
        "writer must be able to write the ingest-accounting tables"
    );
    assert!(!report.read_role_can_write);

    // Independently, AS the writer role over a pure ConnectionConfig: a rolled-back ingest_run +
    // ingest_member insert (the latter allocates from `public.ingest_member_member_id_seq`) succeeds.
    let writer = target_config(&admin, "ext_ingest", "jurisearch_write");
    let mut writer_client = writer.connect()?;
    {
        let mut tx = writer_client
            .transaction()
            .map_err(StorageError::PostgresClient)?;
        tx.execute(
            "INSERT INTO public.ingest_run \
               (run_id, source, status, parser_version, schema_version, code_version) \
             VALUES ('it-probe', 'src', 'running', 'p', 's', 'c');",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
        let member_id: i64 = tx
            .query_one(
                "INSERT INTO public.ingest_member \
                   (run_id, archive_name, member_path, source, status, parser_version, \
                    schema_version, code_version, source_payload_hash) \
                 VALUES ('it-probe', 'a', 'm', 'src', 'discovered', 'p', 's', 'c', 'h') \
                 RETURNING member_id;",
                &[],
            )
            .map_err(StorageError::PostgresClient)?
            .get(0);
        assert!(member_id > 0, "bigserial member_id must allocate");
        tx.rollback().map_err(StorageError::PostgresClient)?;
    }

    // The read role must be denied the same ingest_run insert with SQLSTATE 42501.
    let read = target_config(&admin, "ext_ingest", "jurisearch_read");
    let mut read_client = read.connect()?;
    let err = read_client
        .execute(
            "INSERT INTO public.ingest_run \
               (run_id, source, status, parser_version, schema_version, code_version) \
             VALUES ('evil', 'src', 'running', 'p', 's', 'c');",
            &[],
        )
        .expect_err("read role must be denied the ingest_run insert");
    assert!(
        err.as_db_error()
            .is_some_and(|db| db.code() == &postgres::error::SqlState::INSUFFICIENT_PRIVILEGE),
        "expected 42501 insufficient_privilege, got {err:?}"
    );
    Ok(())
}

/// Acceptance gate (codex r2 BLOCKER fix): the EXTERNAL PRODUCER writer can write the REPLICATED public
/// WORKING tables the corpus build mutates — `public.documents` AND `public.official_api_responses` (the
/// latter drawing from its `bigserial` `response_id` public sequence) — proving the producer profile
/// granted DML across the FULL public working schema + USAGE on every public sequence, NOT the site's
/// narrow enumerated surface. The read role must STILL be denied the same writes.
#[test]
fn producer_writer_can_write_replicated_public_working_tables_and_read_cannot()
-> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("producer replicated-table write")? else {
        return Ok(());
    };
    let postgres = ManagedPostgres::start_temp(pg_config)?;
    let admin = admin_config(&postgres, &postgres.database);
    let cfg = ProvisionConfig {
        admin: admin.clone(),
        target_db: "ext_producer".to_owned(),
        roles: RoleSpec::default(),
    };
    let report = provision_external_db(&cfg).expect("provisioning must succeed");
    // The strengthened postcondition probe already exercised a replicated working-table write through
    // the writer role (`official_api_responses`); assert it held and the read role was denied.
    assert!(
        report.writer_can_write,
        "producer writer must be able to write the replicated public working tables"
    );
    assert!(!report.read_role_can_write);

    // Independently, AS the writer role over a pure ConnectionConfig: rolled-back inserts into two
    // replicated public working tables succeed (a site-style enumerated grant would deny both).
    let writer = target_config(&admin, "ext_producer", "jurisearch_write");
    let mut writer_client = writer.connect()?;
    {
        let mut tx = writer_client
            .transaction()
            .map_err(StorageError::PostgresClient)?;
        tx.execute(
            "INSERT INTO public.documents \
               (document_id, source, kind, source_uid, body, source_payload_hash) \
             VALUES ('prod-probe', 'cass', 'decision', 'prod-probe', 'corps', 'sha256:p');",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
        let response_id: i64 = tx
            .query_one(
                "INSERT INTO public.official_api_responses \
                   (provider, endpoint, http_method, request_fingerprint, outcome, \
                    response_body_sha256, corpus) \
                 VALUES ('local', 'prod-probe', 'LOCAL', 'prod-probe', 'ok', 'probe', 'core') \
                 RETURNING response_id;",
                &[],
            )
            .map_err(StorageError::PostgresClient)?
            .get(0);
        assert!(response_id > 0, "bigserial response_id must allocate");
        tx.rollback().map_err(StorageError::PostgresClient)?;
    }

    // The read role must be denied a replicated public working-table write with SQLSTATE 42501.
    let read = target_config(&admin, "ext_producer", "jurisearch_read");
    let mut read_client = read.connect()?;
    let err = read_client
        .execute(
            "INSERT INTO public.documents \
               (document_id, source, kind, source_uid, body, source_payload_hash) \
             VALUES ('evil', 'cass', 'decision', 'evil', 'corps', 'sha256:e');",
            &[],
        )
        .expect_err("read role must be denied the documents insert");
    assert!(
        err.as_db_error()
            .is_some_and(|db| db.code() == &postgres::error::SqlState::INSUFFICIENT_PRIVILEGE),
        "expected 42501 insufficient_privilege, got {err:?}"
    );
    Ok(())
}
