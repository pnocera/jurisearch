//! work/09 P2A — least-privilege role identities + the activation read-visibility postcondition,
//! exercised on the managed-PG harness with REAL roles (skips cleanly when `JURISEARCH_PG_CONFIG` is
//! absent).

mod common;

use common::discover_pg_config;
use jurisearch_storage::{
    backend::{
        DEFAULT_OWNER_ROLE, DEFAULT_READ_ROLE, DEFAULT_WRITER_ROLE, ManagedPostgresBackend,
        RoleSpec, StorageBackend, provision_roles,
    },
    generations::{
        ActivationReadVisibility, ActivationStamps, CursorGuard, REPLICATED_TABLES,
        activate_generation_with_guard_and_visibility, create_generation_schema, generation_schema,
        populate_generation_from_public,
    },
    runtime::{ManagedPostgres, StorageError},
};

fn stamps() -> ActivationStamps<'static> {
    ActivationStamps {
        sequence: 1,
        baseline_id: "core-2026-06-27-g0001",
        schema_version: 24,
        embedding_fingerprint: "bge-m3:1024:cls:normalize=true",
        builder_versions: &serde_json::Value::Null,
        last_package_id: None,
        last_package_digest: None,
    }
}

fn seed_one_document(postgres: &ManagedPostgres) -> Result<(), StorageError> {
    postgres.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:ROLE1','cass','decision','cass:ROLE1','Cass','Arret','corps', \
           '2024-01-01','sha256:r1','{}');",
    )?;
    Ok(())
}

fn provision_with(postgres: &ManagedPostgres, spec: &RoleSpec) -> Result<(), StorageError> {
    let mut superuser = postgres.client()?;
    provision_roles(&mut superuser, spec, &postgres.database)?;
    Ok(())
}

fn build_generation(postgres: &ManagedPostgres) -> Result<String, StorageError> {
    let mut superuser = postgres.client()?;
    let generation = create_generation_schema(&mut superuser, "core", 1, Some("core-g0001"))?;
    populate_generation_from_public(&mut superuser, "core", &generation)?;
    Ok(generation)
}

/// Assert `sql` is rejected specifically with PostgreSQL SQLSTATE `42501` (insufficient_privilege), so
/// the denial is a *privilege* failure and not, say, a NOT NULL / syntax error masquerading as one.
fn assert_denied(client: &mut postgres::Client, sql: &str) {
    let error = client
        .execute(sql, &[])
        .expect_err(&format!("read role must be denied: `{sql}`"));
    let denied = error
        .as_db_error()
        .map(|db| db.code() == &postgres::error::SqlState::INSUFFICIENT_PRIVILEGE)
        .unwrap_or(false);
    assert!(
        denied,
        "expected insufficient_privilege (42501) for `{sql}`, got {error:?}"
    );
}

#[test]
fn read_role_is_select_only_and_sees_the_active_topology() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("shared-server read role")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-roles.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    provision_with(&postgres, &RoleSpec::default())?;
    seed_one_document(&postgres)?;
    let generation = build_generation(&postgres)?;
    activate_generation_with_guard_and_visibility(
        &postgres,
        "core",
        &generation,
        &stamps(),
        CursorGuard::FirstBaseline,
        &[],
        &ActivationReadVisibility {
            read_role: DEFAULT_READ_ROLE,
            view_owner_role: DEFAULT_OWNER_ROLE,
        },
    )?;

    let backend = ManagedPostgresBackend::new(
        &postgres,
        DEFAULT_READ_ROLE,
        DEFAULT_WRITER_ROLE,
        DEFAULT_OWNER_ROLE,
    );
    let mut read = backend.read_handle()?.client()?;
    let physical = generation_schema("core", 1);

    // The read identity CAN read the control/manifest surface.
    for sql in [
        "SELECT 1 FROM jurisearch_control.corpus_state WHERE corpus='core'",
        "SELECT 1 FROM jurisearch_control.generation_registry",
        "SELECT 1 FROM public.index_manifest",
    ] {
        read.query(sql, &[])
            .map_err(StorageError::PostgresClient)
            .unwrap_or_else(|error| panic!("read should be able to run `{sql}`: {error:?}"));
    }
    // ...and EVERY replicated relation, both through the stable view and the physical generation —
    // the full-topology postcondition, not just `documents`.
    for table in REPLICATED_TABLES {
        for sql in [
            format!("SELECT count(*) FROM jurisearch_server.{table}"),
            format!("SELECT count(*) FROM {physical}.{table}"),
        ] {
            read.query(&sql, &[])
                .map_err(StorageError::PostgresClient)
                .unwrap_or_else(|error| panic!("read should be able to run `{sql}`: {error:?}"));
        }
    }
    // The seeded row is visible through the stable view (data path, not just relation privilege).
    let count: i64 = read
        .query_one(
            "SELECT count(*) FROM jurisearch_server.documents WHERE document_id='cass:ROLE1'",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    assert_eq!(count, 1);

    // The read identity CANNOT write — VALID statements (so the only possible failure is privilege),
    // each asserted to fail with SQLSTATE 42501.
    let denied = [
        "INSERT INTO jurisearch_control.corpus_state(corpus,active_generation,sequence,baseline_id,\
         schema_version,embedding_fingerprint) VALUES('evil','g',1,'b',1,'f')".to_owned(),
        "UPDATE jurisearch_control.corpus_state SET sequence=99 WHERE corpus='core'".to_owned(),
        "DELETE FROM jurisearch_control.corpus_state".to_owned(),
        "INSERT INTO public.index_manifest(key,value) VALUES('evil','{}'::jsonb)".to_owned(),
        "CREATE TABLE public.evil_table (x int)".to_owned(),
        "CREATE SCHEMA evil_schema".to_owned(),
        format!(
            "INSERT INTO {physical}.documents (document_id, source, kind, source_uid, citation, \
               title, body, valid_from, source_payload_hash, canonical_json) \
             VALUES ('evil','cass','decision','evil','c','t','b','2024-01-01','sha256:e','{{}}')"
        ),
    ];
    for sql in &denied {
        assert_denied(&mut read, sql);
    }

    // The stable view exposes SELECT but never INSERT to the read role (it is not an updatable view,
    // so check the privilege catalog directly).
    let mut superuser = postgres.client()?;
    let can_select: bool = superuser
        .query_one(
            "SELECT has_table_privilege($1, 'jurisearch_server.documents', 'SELECT')",
            &[&DEFAULT_READ_ROLE],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    let can_insert: bool = superuser
        .query_one(
            "SELECT has_table_privilege($1, 'jurisearch_server.documents', 'INSERT')",
            &[&DEFAULT_READ_ROLE],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    assert!(can_select, "read should hold SELECT on the stable view");
    assert!(!can_insert, "read must NOT hold INSERT on the stable view");

    Ok(())
}

#[test]
fn activation_with_unusable_read_role_aborts_with_cursor_unchanged() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("shared-server visibility abort")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-roles-abort.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    provision_with(&postgres, &RoleSpec::default())?;
    seed_one_document(&postgres)?;
    let generation = build_generation(&postgres)?;

    // Activate the FIRST baseline but request visibility for a role that does not exist: the grant /
    // probe fails inside the switch transaction, so the whole switch rolls back.
    let result = activate_generation_with_guard_and_visibility(
        &postgres,
        "core",
        &generation,
        &stamps(),
        CursorGuard::FirstBaseline,
        &[],
        &ActivationReadVisibility {
            read_role: "definitely_not_a_role",
            view_owner_role: DEFAULT_OWNER_ROLE,
        },
    );
    assert!(
        result.is_err(),
        "activation must fail for an unusable read role"
    );

    // Cursor unchanged: no `corpus_state` row was written.
    let cursor_rows = postgres.execute_sql(
        "SELECT count(*)::text FROM jurisearch_control.corpus_state WHERE corpus='core';",
    )?;
    assert_eq!(
        cursor_rows.trim(),
        "0",
        "cursor must be unchanged after abort"
    );

    // The generation is never left active — it stays `building`.
    let state = postgres.execute_sql(&format!(
        "SELECT state FROM jurisearch_control.generation_registry \
         WHERE corpus='core' AND generation='{generation}';"
    ))?;
    assert_eq!(
        state.trim(),
        "building",
        "generation must not be left active"
    );

    Ok(())
}

#[test]
fn provisioning_converges_a_preexisting_overprivileged_read_role() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("shared-server convergence")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-roles-converge.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // A pre-existing deployment where the read role is OVER-privileged and the legacy `PUBLIC` CREATE
    // surface on `public` is present.
    {
        let mut superuser = postgres.client()?;
        superuser
            .batch_execute(&format!(
                "CREATE ROLE {read} LOGIN SUPERUSER CREATEDB CREATEROLE;\n\
                 GRANT CREATE ON SCHEMA public TO PUBLIC;\n\
                 GRANT INSERT, UPDATE, DELETE ON jurisearch_control.corpus_state TO {read};",
                read = DEFAULT_READ_ROLE,
            ))
            .map_err(StorageError::PostgresClient)?;
    }

    // Provisioning must CONVERGE it back to least privilege, not leave the extra privileges in place.
    provision_with(&postgres, &RoleSpec::default())?;

    let mut superuser = postgres.client()?;
    let attrs = superuser
        .query_one(
            "SELECT rolsuper, rolcreatedb, rolcreaterole FROM pg_roles WHERE rolname = $1",
            &[&DEFAULT_READ_ROLE],
        )
        .map_err(StorageError::PostgresClient)?;
    assert!(!attrs.get::<_, bool>(0), "read must no longer be superuser");
    assert!(!attrs.get::<_, bool>(1), "read must no longer be createdb");
    assert!(
        !attrs.get::<_, bool>(2),
        "read must no longer be createrole"
    );

    let backend = ManagedPostgresBackend::new(
        &postgres,
        DEFAULT_READ_ROLE,
        DEFAULT_WRITER_ROLE,
        DEFAULT_OWNER_ROLE,
    );
    let mut read = backend.read_handle()?.client()?;
    assert_denied(&mut read, "CREATE TABLE public.evil_after (x int)");
    assert_denied(
        &mut read,
        "INSERT INTO jurisearch_control.corpus_state(corpus,active_generation,sequence,baseline_id,\
         schema_version,embedding_fingerprint) VALUES('evil','g',1,'b',1,'f')",
    );

    Ok(())
}

#[test]
fn provisioning_strips_inherited_and_set_role_write_paths() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("shared-server membership strip")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-roles-membership.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // A pre-existing deployment where the read login is a MEMBER of a separate write-capable role —
    // a write path provisioning must remove, both the inherited one and the `SET ROLE` one.
    {
        let mut superuser = postgres.client()?;
        superuser
            .batch_execute(&format!(
                "CREATE ROLE rogue_writer NOLOGIN;\n\
                 GRANT INSERT, UPDATE, DELETE ON jurisearch_control.corpus_state TO rogue_writer;\n\
                 CREATE ROLE {read} LOGIN INHERIT;\n\
                 GRANT rogue_writer TO {read};",
                read = DEFAULT_READ_ROLE,
            ))
            .map_err(StorageError::PostgresClient)?;
    }

    provision_with(&postgres, &RoleSpec::default())?;

    // The membership is gone.
    let mut superuser = postgres.client()?;
    let still_member: bool = superuser
        .query_one(
            "SELECT pg_has_role($1, 'rogue_writer', 'MEMBER')",
            &[&DEFAULT_READ_ROLE],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    assert!(
        !still_member,
        "read must not retain the rogue_writer membership"
    );

    let backend = ManagedPostgresBackend::new(
        &postgres,
        DEFAULT_READ_ROLE,
        DEFAULT_WRITER_ROLE,
        DEFAULT_OWNER_ROLE,
    );
    let mut read = backend.read_handle()?.client()?;
    // The inherited write path is gone.
    assert_denied(
        &mut read,
        "INSERT INTO jurisearch_control.corpus_state(corpus,active_generation,sequence,baseline_id,\
         schema_version,embedding_fingerprint) VALUES('evil','g',1,'b',1,'f')",
    );
    // And the `SET ROLE` elevation path is gone (read is no longer a member, so it cannot assume it).
    assert!(
        read.batch_execute("SET ROLE rogue_writer").is_err(),
        "read must not be able to SET ROLE into rogue_writer"
    );

    Ok(())
}

#[test]
fn provisioning_and_activation_handle_non_simple_role_names() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("shared-server non-simple roles")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-roles-quoting.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // Hyphenated + mixed-case names force identifier quoting through EVERY path: role creation, grants,
    // the ownership DO-block (`%I`), the activation grants, the view-owner chain, and the connection.
    let spec = RoleSpec {
        read_role: "Juris-Read".to_owned(),
        writer_role: "Juris-Write".to_owned(),
        owner_role: "Juris-Owner".to_owned(),
        read_password: None,
        writer_password: None,
    };
    provision_with(&postgres, &spec)?;
    seed_one_document(&postgres)?;
    let generation = build_generation(&postgres)?;
    activate_generation_with_guard_and_visibility(
        &postgres,
        "core",
        &generation,
        &stamps(),
        CursorGuard::FirstBaseline,
        &[],
        &ActivationReadVisibility {
            read_role: &spec.read_role,
            view_owner_role: &spec.owner_role,
        },
    )?;

    let backend = ManagedPostgresBackend::new(
        &postgres,
        &spec.read_role,
        &spec.writer_role,
        &spec.owner_role,
    );
    let mut read = backend.read_handle()?.client()?;
    let count: i64 = read
        .query_one(
            "SELECT count(*) FROM jurisearch_server.documents WHERE document_id='cass:ROLE1'",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    assert_eq!(
        count, 1,
        "read through the view must work for a quoted role"
    );

    Ok(())
}

#[test]
fn writer_public_select_is_scoped_to_replicated_templates() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("shared-server writer public scope")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-roles-pubscope.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // A pre-existing deployment where the writer already holds the OLD broad `public` SELECT (the
    // exposure this fix closes), including an unrelated table provisioning must converge away.
    {
        let mut superuser = postgres.client()?;
        superuser
            .batch_execute(&format!(
                "CREATE TABLE public.unrelated_secret (x int);\n\
                 CREATE ROLE {DEFAULT_WRITER_ROLE} LOGIN;\n\
                 GRANT SELECT ON ALL TABLES IN SCHEMA public TO {DEFAULT_WRITER_ROLE};"
            ))
            .map_err(StorageError::PostgresClient)?;
    }
    provision_with(&postgres, &RoleSpec::default())?;

    // The writer holds SELECT on the replicated `LIKE` template but NOT on the unrelated table.
    // (A writer baseline against the templates succeeds — see `shared_writer_loopback`.)
    let mut superuser = postgres.client()?;
    let can_template: bool = superuser
        .query_one(
            "SELECT has_table_privilege($1, 'public.documents', 'SELECT')",
            &[&DEFAULT_WRITER_ROLE],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    let can_secret: bool = superuser
        .query_one(
            "SELECT has_table_privilege($1, 'public.unrelated_secret', 'SELECT')",
            &[&DEFAULT_WRITER_ROLE],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    assert!(
        can_template,
        "writer needs SELECT on the replicated template"
    );
    assert!(
        !can_secret,
        "writer must NOT read an unrelated public table"
    );

    Ok(())
}

#[test]
fn provisioning_strips_a_preexisting_admin_option_on_writer_memberships() -> Result<(), StorageError>
{
    let Some(pg_config) = discover_pg_config("shared-server admin-option strip")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-roles-admin.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // A drifted deployment: the writer is a member of owner + read WITH ADMIN OPTION (could re-delegate).
    {
        let mut superuser = postgres.client()?;
        superuser
            .batch_execute(&format!(
                "CREATE ROLE {DEFAULT_OWNER_ROLE} NOLOGIN;\n\
                 CREATE ROLE {DEFAULT_READ_ROLE} LOGIN;\n\
                 CREATE ROLE {DEFAULT_WRITER_ROLE} LOGIN;\n\
                 GRANT {DEFAULT_OWNER_ROLE} TO {DEFAULT_WRITER_ROLE} WITH ADMIN OPTION;\n\
                 GRANT {DEFAULT_READ_ROLE} TO {DEFAULT_WRITER_ROLE} WITH ADMIN OPTION;"
            ))
            .map_err(StorageError::PostgresClient)?;
    }

    provision_with(&postgres, &RoleSpec::default())?;

    // Each writer membership is retained but the admin option is gone.
    let mut superuser = postgres.client()?;
    for group in [DEFAULT_OWNER_ROLE, DEFAULT_READ_ROLE] {
        let admin: bool = superuser
            .query_one(
                "SELECT coalesce(bool_or(admin_option), false) FROM pg_auth_members \
                 WHERE roleid = to_regrole($1) AND member = to_regrole($2)",
                &[&group, &DEFAULT_WRITER_ROLE],
            )
            .map_err(StorageError::PostgresClient)?
            .get(0);
        assert!(!admin, "admin option on {group}->writer must be stripped");
        let member: bool = superuser
            .query_one(
                "SELECT pg_has_role($1, $2, 'MEMBER')",
                &[&DEFAULT_WRITER_ROLE, &group],
            )
            .map_err(StorageError::PostgresClient)?
            .get(0);
        assert!(member, "writer must remain a member of {group}");
    }

    // The writer can still assume the read role (the activation `SET ROLE` probe path).
    let backend = ManagedPostgresBackend::new(
        &postgres,
        DEFAULT_READ_ROLE,
        DEFAULT_WRITER_ROLE,
        DEFAULT_OWNER_ROLE,
    );
    let mut writer = backend.writer_handle()?.client()?;
    writer
        .batch_execute(&format!("SET ROLE {DEFAULT_READ_ROLE}; RESET ROLE;"))
        .map_err(StorageError::PostgresClient)?;

    Ok(())
}
