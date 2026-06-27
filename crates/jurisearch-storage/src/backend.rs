//! Storage backend + least-privilege role identities (work/09 P2A).
//!
//! One authority for "where is the database and how do I get a role-scoped handle" ([`StorageBackend`],
//! design §3.2), so the query service literally holds a **read-only** identity and the writer holds a
//! **writer** identity — least-privilege as a type-level fact, not a convention.
//!
//! Two concretions, substitutable behind the trait (LSP):
//! * [`SharedServerBackend`] — attaches to an existing PostgreSQL as a client (the customer-site host);
//! * [`ManagedPostgresBackend`] — the self-managed `pg_ctl`-owned PG (producer / tests).
//!
//! 2A scope is the role *identities* + the activation read-visibility grant, NOT a bounded connection
//! pool: [`ReadHandle`]/[`WriterHandle`] are role-scoped connection **providers** (open a fresh
//! `postgres::Client`). The bounded worker/connection pool (substrate A) is P4's concern and can wrap
//! the same [`ConnectionConfig`] without changing the role model.

use std::time::Duration;

use crate::generations::{ActivationReadVisibility, REPLICATED_TABLES};
use crate::runtime::{ManagedPostgres, StorageError, sql_identifier, sql_string_literal};

/// Default least-privilege role names. A deployment may override them via [`RoleSpec`].
pub const DEFAULT_READ_ROLE: &str = "jurisearch_read";
pub const DEFAULT_WRITER_ROLE: &str = "jurisearch_write";
/// NOLOGIN role that *owns* the app-managed namespaces, so the writer (a member) can run the dynamic
/// DDL (replace stable views, create generation schemas) without being a superuser or owning `public`.
pub const DEFAULT_OWNER_ROLE: &str = "jurisearch_owner";

/// libpq connection parameters for one role identity. `Debug` redacts the password.
#[derive(Clone)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub dbname: String,
    pub user: String,
    pub password: Option<String>,
    /// Surfaces the connecting identity in `pg_stat_activity` so a role/connection mistake is visible.
    pub application_name: String,
}

impl std::fmt::Debug for ConnectionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("dbname", &self.dbname)
            .field("user", &self.user)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("application_name", &self.application_name)
            .finish()
    }
}

impl ConnectionConfig {
    /// Open a fresh libpq client with this identity. (NoTls — the site LAN is the trust boundary, §6;
    /// TLS termination is an operator/perimeter concern, out of scope here.)
    pub fn connect(&self) -> Result<postgres::Client, StorageError> {
        let mut config = postgres::Config::new();
        config
            .host(&self.host)
            .port(self.port)
            .dbname(&self.dbname)
            .user(&self.user)
            .application_name(&self.application_name)
            .connect_timeout(Duration::from_secs(5));
        if let Some(password) = &self.password {
            config.password(password);
        }
        config
            .connect(postgres::NoTls)
            .map_err(StorageError::PostgresClient)
    }
}

/// A read-only connection provider. Disjoint from [`WriterHandle`] (ISP): it exposes no writer access.
#[derive(Debug, Clone)]
pub struct ReadHandle {
    config: ConnectionConfig,
}

impl ReadHandle {
    pub fn new(config: ConnectionConfig) -> Self {
        Self { config }
    }
    pub fn config(&self) -> &ConnectionConfig {
        &self.config
    }
    /// Open a fresh client with the **read-only** identity.
    pub fn client(&self) -> Result<postgres::Client, StorageError> {
        self.config.connect()
    }
}

/// The read/owner role names a writer must propagate visibility to when it activates a generation
/// (work/09 P2B). Owned (vs the borrowed [`ActivationReadVisibility`]) so a [`WriterHandle`] can carry
/// it across an apply. Absent for the self-managed (no-roles) path.
#[derive(Debug, Clone)]
pub struct WriterVisibility {
    pub read_role: String,
    pub view_owner_role: String,
}

/// A writer connection provider. Disjoint from [`ReadHandle`] (ISP). A `WriterHandle` is the
/// **shared-server** writer identity, so it ALWAYS carries the [`WriterVisibility`] role names — an
/// activation run through it stamps read-role visibility (the P2A postcondition). (The self-managed
/// superuser path is `ManagedPostgres`'s `WriterConnection` impl, whose visibility is `None`.)
#[derive(Debug, Clone)]
pub struct WriterHandle {
    config: ConnectionConfig,
    visibility: WriterVisibility,
}

impl WriterHandle {
    pub fn new(config: ConnectionConfig, visibility: WriterVisibility) -> Self {
        Self { config, visibility }
    }
    pub fn config(&self) -> &ConnectionConfig {
        &self.config
    }
    /// Open a fresh client with the **writer** identity.
    pub fn client(&self) -> Result<postgres::Client, StorageError> {
        self.config.connect()
    }
}

/// One authority for "give me a WRITER connection, and (for a shared server) the read-role visibility
/// to stamp at activation" (work/09 P2B). Implemented by both [`ManagedPostgres`] (the self-managed
/// superuser adapter — `None` visibility) and [`WriterHandle`] (the writer role — `Some` visibility),
/// so the syncd writer path runs unchanged against either, and the storage activation can open its
/// switch connection through the right identity instead of a hardcoded superuser string. Object-safe.
pub trait WriterConnection {
    /// Open a fresh client with the writer identity.
    fn writer_client(&self) -> Result<postgres::Client, StorageError>;
    /// The read-role visibility to stamp at activation, or `None` for the self-managed path.
    fn activation_read_visibility(&self) -> Option<ActivationReadVisibility<'_>>;
}

impl WriterConnection for WriterHandle {
    fn writer_client(&self) -> Result<postgres::Client, StorageError> {
        self.config.connect()
    }
    fn activation_read_visibility(&self) -> Option<ActivationReadVisibility<'_>> {
        Some(ActivationReadVisibility {
            read_role: &self.visibility.read_role,
            view_owner_role: &self.visibility.view_owner_role,
        })
    }
}

impl WriterConnection for ManagedPostgres {
    fn writer_client(&self) -> Result<postgres::Client, StorageError> {
        self.client()
    }
    /// The self-managed path uses the superuser identity and provisions no roles, so there is no
    /// read-role visibility to stamp.
    fn activation_read_visibility(&self) -> Option<ActivationReadVisibility<'_>> {
        None
    }
}

/// One authority for "where is the DB and how do I get a role-scoped handle" (design §3.2). The two
/// handles carry **disjoint** identities, so least-privilege is type-level: the query service can only
/// obtain a [`ReadHandle`]; syncd only a [`WriterHandle`].
pub trait StorageBackend {
    fn read_handle(&self) -> Result<ReadHandle, StorageError>;
    fn writer_handle(&self) -> Result<WriterHandle, StorageError>;
}

/// Attaches to an existing PostgreSQL server as a client (the customer-site host) — no `pg_ctl`, no
/// data dir. Holds a read-only [`ConnectionConfig`], a writer [`ConnectionConfig`], and the
/// [`WriterVisibility`] role names the writer stamps at activation.
#[derive(Debug, Clone)]
pub struct SharedServerBackend {
    read: ConnectionConfig,
    writer: ConnectionConfig,
    visibility: WriterVisibility,
}

impl SharedServerBackend {
    pub fn new(
        read: ConnectionConfig,
        writer: ConnectionConfig,
        visibility: WriterVisibility,
    ) -> Self {
        Self {
            read,
            writer,
            visibility,
        }
    }
}

impl StorageBackend for SharedServerBackend {
    fn read_handle(&self) -> Result<ReadHandle, StorageError> {
        Ok(ReadHandle::new(self.read.clone()))
    }
    fn writer_handle(&self) -> Result<WriterHandle, StorageError> {
        Ok(WriterHandle::new(
            self.writer.clone(),
            self.visibility.clone(),
        ))
    }
}

/// The self-managed (`pg_ctl`-owned) PG presented through the same backend interface, so the producer
/// and tests can exercise the role-scoped handles on the managed harness. Built from the managed PG's
/// loopback address with the read/writer role identities (the managed harness uses `trust` auth, so a
/// password is optional — the role *privileges* are still enforced).
#[derive(Debug, Clone)]
pub struct ManagedPostgresBackend {
    read: ConnectionConfig,
    writer: ConnectionConfig,
    visibility: WriterVisibility,
}

impl ManagedPostgresBackend {
    /// Build role-scoped configs against `postgres`'s loopback address for the given role names. The
    /// `owner_role` (the role that owns the stable views) is carried as the activation view-owner.
    pub fn new(
        postgres: &ManagedPostgres,
        read_role: &str,
        writer_role: &str,
        owner_role: &str,
    ) -> Self {
        let base = |user: &str, app: &str| ConnectionConfig {
            host: "127.0.0.1".to_owned(),
            port: postgres.port,
            dbname: postgres.database.clone(),
            user: user.to_owned(),
            password: None,
            application_name: app.to_owned(),
        };
        Self {
            read: base(read_role, "jurisearch-read"),
            writer: base(writer_role, "jurisearch-write"),
            visibility: WriterVisibility {
                read_role: read_role.to_owned(),
                view_owner_role: owner_role.to_owned(),
            },
        }
    }
}

impl StorageBackend for ManagedPostgresBackend {
    fn read_handle(&self) -> Result<ReadHandle, StorageError> {
        Ok(ReadHandle::new(self.read.clone()))
    }
    fn writer_handle(&self) -> Result<WriterHandle, StorageError> {
        Ok(WriterHandle::new(
            self.writer.clone(),
            self.visibility.clone(),
        ))
    }
}

/// The role identities a deployment provisions. Names default to the `jurisearch_*` set; passwords are
/// optional (the managed harness uses `trust` auth — a real shared server sets real passwords). `Debug`
/// redacts the passwords (mirrors [`ConnectionConfig`]).
#[derive(Clone)]
pub struct RoleSpec {
    pub read_role: String,
    pub writer_role: String,
    pub owner_role: String,
    pub read_password: Option<String>,
    pub writer_password: Option<String>,
}

impl std::fmt::Debug for RoleSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleSpec")
            .field("read_role", &self.read_role)
            .field("writer_role", &self.writer_role)
            .field("owner_role", &self.owner_role)
            .field(
                "read_password",
                &self.read_password.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "writer_password",
                &self.writer_password.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl Default for RoleSpec {
    fn default() -> Self {
        Self {
            read_role: DEFAULT_READ_ROLE.to_owned(),
            writer_role: DEFAULT_WRITER_ROLE.to_owned(),
            owner_role: DEFAULT_OWNER_ROLE.to_owned(),
            read_password: None,
            writer_password: None,
        }
    }
}

/// Provision the least-privilege roles + grants on a freshly-migrated database (idempotent;
/// superuser connection). This is a **provisioning** step (role names/passwords are deployment config,
/// not a hardcoded schema migration). It:
///
/// 1. creates the NOLOGIN owner + LOGIN read/writer roles (idempotent), and makes the writer a member
///    of the owner so it can run the app's dynamic DDL without superuser;
/// 2. transfers ownership of the app-managed namespaces (`jurisearch_control` / `jurisearch_server` /
///    `jurisearch_app`) and the app-owned `public` objects to the owner, so the writer (member) can
///    `CREATE OR REPLACE` the stable views and create generation schemas later (2B);
/// 3. grants the **read** role SELECT-only on the control/manifest tables + the stable views (and a
///    default privilege so rebuilt views stay readable) — and **no** write anywhere;
/// 4. grants the **writer** role the DML + CREATE-on-database it needs to apply.
///
/// The per-generation grant (read role on each newly activated physical schema) is NOT here — it is an
/// activation postcondition (`activate_generation_with_guard_and_visibility`).
pub fn provision_roles(
    client: &mut postgres::Client,
    spec: &RoleSpec,
    dbname: &str,
) -> Result<(), StorageError> {
    let sql = build_provision_sql(spec, dbname);
    client
        .batch_execute(&sql)
        .map_err(StorageError::PostgresClient)
}

/// Build the **convergent**, idempotent provisioning DDL. Role names are identifier-quoted; the
/// `pg_roles` existence checks / passwords / the dynamic owner are string-literal-quoted (and re-quoted
/// as identifiers at runtime with `%I`). It does not merely *add* privileges — it normalizes role
/// attributes and revokes accidental surfaces first, so an over-privileged pre-existing role (or a
/// legacy `PUBLIC` CREATE on `public`) is brought back down to least privilege, not left in place.
fn build_provision_sql(spec: &RoleSpec, dbname: &str) -> String {
    let owner = sql_identifier(&spec.owner_role);
    let read = sql_identifier(&spec.read_role);
    let writer = sql_identifier(&spec.writer_role);
    let db = sql_identifier(dbname);
    let owner_lit = sql_string_literal(&spec.owner_role);
    // Every app-managed namespace the read role's access is converged over.
    const APP_SCHEMAS: &str = "public, jurisearch_control, jurisearch_server, jurisearch_app";

    // Idempotent CREATE ROLE via a DO block: the role name is matched as a string literal in the
    // existence check and identifier-quoted at runtime with `quote_ident`.
    let create_role = |role_lit: &str, options: &str| {
        format!(
            "DO $do$ BEGIN \
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = {role_lit}) THEN \
                 EXECUTE 'CREATE ROLE ' || quote_ident({role_lit}) || ' {options}'; \
               END IF; \
             END $do$;\n",
        )
    };

    let mut sql = String::new();

    // 1. Roles (idempotent create) + attribute NORMALIZATION so a pre-existing over-privileged role is
    //    converged down. Owner NOLOGIN; read/writer LOGIN; none super/createdb/createrole/replication.
    sql.push_str(&create_role(&owner_lit, "NOLOGIN"));
    sql.push_str(&create_role(&sql_string_literal(&spec.read_role), "LOGIN"));
    sql.push_str(&create_role(
        &sql_string_literal(&spec.writer_role),
        "LOGIN",
    ));
    sql.push_str(&format!(
        "ALTER ROLE {owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;\n"
    ));
    // The read role is also NOINHERIT (defense in depth): even if a membership were re-introduced, it
    // would not auto-inherit that role's privileges. The real guarantee is the membership revoke in
    // step 5; this just narrows the blast radius.
    sql.push_str(&format!(
        "ALTER ROLE {read} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT;\n"
    ));
    sql.push_str(&format!(
        "ALTER ROLE {writer} LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS;\n"
    ));
    // The writer is a member of the owner role (to run the app DDL) and a member of the READ role
    // (ONLY so it can `SET LOCAL ROLE <read>` for the in-transaction activation visibility probe when
    // activation runs as the writer rather than a superuser). Both are read/owner→writer (the writer
    // *assumes* the narrower identity), never writer→read. CONVERGENT: each membership is granted
    // WITHOUT admin option, and a pre-existing ADMIN OPTION is explicitly stripped, so the writer can
    // never re-delegate these roles even on a drifted deployment. (The step-5 convergence loop strips
    // memberships where READ is the *member*; these have read/owner as the *group*, so they survive.)
    sql.push_str(&format!("GRANT {owner} TO {writer};\n"));
    sql.push_str(&format!("REVOKE ADMIN OPTION FOR {owner} FROM {writer};\n"));
    sql.push_str(&format!("GRANT {read} TO {writer};\n"));
    sql.push_str(&format!("REVOKE ADMIN OPTION FOR {read} FROM {writer};\n"));

    // 2. Remove the legacy `PUBLIC` CREATE on `public` so no role (incl. read) can create there via
    //    PUBLIC. (PG15+ already revokes this by default; the revoke is idempotent on newer clusters.)
    sql.push_str("REVOKE CREATE ON SCHEMA public FROM PUBLIC;\n");

    // 3. Database-level: the writer connects + may CREATE (generation schemas are dynamic). The read
    //    role's CONNECT is (re-)granted in step 5 after its converge-revoke.
    sql.push_str(&format!(
        "GRANT CONNECT, CREATE ON DATABASE {db} TO {writer};\n"
    ));

    // 4. Transfer ownership of the app-managed namespaces + their objects to the owner role, so the
    //    writer (member) can replace the stable views and manage app objects. Migration 20 already
    //    created the `jurisearch_server` views (owned by the bootstrap superuser) — ownership must move
    //    or 2B activation (run as the writer) cannot `CREATE OR REPLACE` them. The owner role name is
    //    bound as a literal and re-quoted as an identifier via `%I` (safe for any configured name).
    for schema in ["jurisearch_control", "jurisearch_server", "jurisearch_app"] {
        sql.push_str(&format!(
            "ALTER SCHEMA {} OWNER TO {owner};\n",
            sql_identifier(schema)
        ));
    }
    sql.push_str(&format!(
        "DO $do$ DECLARE r record; owner_name text := {owner_lit}; BEGIN \
           FOR r IN SELECT schemaname, tablename FROM pg_tables \
                    WHERE schemaname IN ('jurisearch_control','jurisearch_app') LOOP \
             EXECUTE format('ALTER TABLE %I.%I OWNER TO %I', r.schemaname, r.tablename, owner_name); \
           END LOOP; \
           FOR r IN SELECT schemaname, viewname FROM pg_views WHERE schemaname = 'jurisearch_server' LOOP \
             EXECUTE format('ALTER VIEW %I.%I OWNER TO %I', r.schemaname, r.viewname, owner_name); \
           END LOOP; \
           FOR r IN SELECT sequence_schema, sequence_name FROM information_schema.sequences \
                    WHERE sequence_schema IN ('jurisearch_control','jurisearch_app') LOOP \
             EXECUTE format('ALTER SEQUENCE %I.%I OWNER TO %I', r.sequence_schema, r.sequence_name, owner_name); \
           END LOOP; \
         END $do$;\n",
    ));
    for table in [
        "public.index_manifest",
        "public.schema_migrations",
        "public.package_change_log",
        "public.package_catalog",
    ] {
        sql.push_str(&format!("ALTER TABLE {table} OWNER TO {owner};\n"));
    }

    // 5. Read role → SELECT-only. CONVERGE: drop EVERY existing membership + every privilege, THEN
    //    grant only the minimal read surface. So a read role that previously had DML / CREATE, or was
    //    a member of any write-capable role (an inherited *or* `SET ROLE` write path), is stripped back
    //    to least privilege — not merely topped up. The membership revoke is dynamic: it strips any
    //    role the read login is a member of, not only the two app roles this provisioner manages.
    sql.push_str(&format!(
        "DO $do$ DECLARE r record; read_name text := {read_lit}; BEGIN \
           FOR r IN SELECT g.rolname AS group_role \
                    FROM pg_auth_members m \
                    JOIN pg_roles mem ON mem.oid = m.member \
                    JOIN pg_roles g ON g.oid = m.roleid \
                    WHERE mem.rolname = read_name LOOP \
             EXECUTE format('REVOKE %I FROM %I', r.group_role, read_name); \
           END LOOP; \
         END $do$;\n",
        read_lit = sql_string_literal(&spec.read_role),
    ));
    sql.push_str(&format!("REVOKE ALL ON DATABASE {db} FROM {read};\n"));
    sql.push_str(&format!(
        "REVOKE ALL ON SCHEMA {APP_SCHEMAS} FROM {read};\n"
    ));
    sql.push_str(&format!(
        "REVOKE ALL ON ALL TABLES IN SCHEMA {APP_SCHEMAS} FROM {read};\n"
    ));
    sql.push_str(&format!(
        "REVOKE ALL ON ALL SEQUENCES IN SCHEMA {APP_SCHEMAS} FROM {read};\n"
    ));
    sql.push_str(&format!("GRANT CONNECT ON DATABASE {db} TO {read};\n"));
    sql.push_str(&format!(
        "GRANT USAGE ON SCHEMA public, jurisearch_control, jurisearch_server TO {read};\n"
    ));
    sql.push_str(&format!(
        "GRANT SELECT ON jurisearch_control.corpus_state, jurisearch_control.generation_registry TO {read};\n"
    ));
    sql.push_str(&format!(
        "GRANT SELECT ON public.index_manifest, public.schema_migrations TO {read};\n"
    ));
    sql.push_str(&format!(
        "GRANT SELECT ON ALL TABLES IN SCHEMA jurisearch_server TO {read};\n"
    ));
    sql.push_str(&format!(
        "ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA jurisearch_server GRANT SELECT ON TABLES TO {read};\n"
    ));

    // 6. Writer role: USAGE + DML on the app namespaces + sequence usage (no ownership of `public`).
    sql.push_str(&format!(
        "GRANT USAGE ON SCHEMA public, jurisearch_control, jurisearch_server, jurisearch_app TO {writer};\n"
    ));
    // The writer creates each generation's tables as `CREATE TABLE … (LIKE public.<table> …)`, which
    // needs SELECT on exactly the `public` REPLICATED templates (and the named control/catalog tables,
    // re-granted below). CONVERGE first: revoke the writer's `public` table surface, so a deployment
    // that previously received a broad `public` grant (or drifted into one) is brought back to exactly
    // the needed set — the writer cannot read an unrelated `public` table. (It writes its corpus data
    // into the per-generation schemas, never into `public`.)
    sql.push_str(&format!(
        "REVOKE ALL ON ALL TABLES IN SCHEMA public FROM {writer};\n"
    ));
    let replicated = REPLICATED_TABLES
        .iter()
        .map(|table| format!("public.{}", sql_identifier(table)))
        .collect::<Vec<_>>()
        .join(", ");
    sql.push_str(&format!("GRANT SELECT ON {replicated} TO {writer};\n"));
    sql.push_str(&format!(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA jurisearch_control, jurisearch_app TO {writer};\n"
    ));
    sql.push_str(&format!(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON public.index_manifest, public.schema_migrations, public.package_change_log, public.package_catalog TO {writer};\n"
    ));
    sql.push_str(&format!(
        "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA jurisearch_control, jurisearch_app TO {writer};\n"
    ));

    // 6. Optional passwords (a real shared server; the managed harness uses trust auth).
    if let Some(password) = &spec.read_password {
        sql.push_str(&format!(
            "ALTER ROLE {read} PASSWORD {};\n",
            sql_string_literal(password)
        ));
    }
    if let Some(password) = &spec.writer_password {
        sql.push_str(&format!(
            "ALTER ROLE {writer} PASSWORD {};\n",
            sql_string_literal(password)
        ));
    }

    sql
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_config_debug_redacts_password() {
        let config = ConnectionConfig {
            host: "127.0.0.1".to_owned(),
            port: 5432,
            dbname: "jurisearch".to_owned(),
            user: "jurisearch_read".to_owned(),
            password: Some("super-secret".to_owned()),
            application_name: "jurisearch-read".to_owned(),
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("super-secret"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn role_spec_debug_redacts_passwords() {
        let spec = RoleSpec {
            read_password: Some("read-secret".to_owned()),
            writer_password: Some("write-secret".to_owned()),
            ..RoleSpec::default()
        };
        let rendered = format!("{spec:?}");
        assert!(!rendered.contains("read-secret"), "{rendered}");
        assert!(!rendered.contains("write-secret"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
    }

    #[test]
    fn provision_sql_quotes_identifiers_and_omits_password_when_absent() {
        let sql = build_provision_sql(&RoleSpec::default(), "jurisearch");
        assert!(sql.contains("\"jurisearch_read\""));
        assert!(sql.contains("\"jurisearch_write\""));
        assert!(sql.contains("GRANT \"jurisearch_owner\" TO \"jurisearch_write\";"));
        // The writer is a (non-admin) member of the read role so it can SET ROLE read for the
        // activation probe (work/09 P2B) — read→writer, never WITH ADMIN OPTION, and any pre-existing
        // admin option is explicitly stripped.
        assert!(sql.contains("GRANT \"jurisearch_read\" TO \"jurisearch_write\";"));
        assert!(!sql.to_uppercase().contains("WITH ADMIN OPTION"));
        assert!(
            sql.contains("REVOKE ADMIN OPTION FOR \"jurisearch_read\" FROM \"jurisearch_write\";")
        );
        assert!(
            sql.contains("REVOKE ADMIN OPTION FOR \"jurisearch_owner\" FROM \"jurisearch_write\";")
        );
        assert!(
            sql.contains(
                "GRANT CONNECT, CREATE ON DATABASE \"jurisearch\" TO \"jurisearch_write\";"
            )
        );
        // The writer's `public` read access is CONVERGED to the replicated `LIKE` templates: its
        // public table surface is revoked first, then SELECT is re-granted on JUST those templates
        // (never all of `public`), so a previously-broad grant is brought back to least privilege.
        assert!(
            sql.contains("REVOKE ALL ON ALL TABLES IN SCHEMA public FROM \"jurisearch_write\";")
        );
        assert!(sql.contains("GRANT SELECT ON public.\"documents\", public.\"chunks\""));
        assert!(
            !sql.contains("GRANT SELECT ON ALL TABLES IN SCHEMA public TO \"jurisearch_write\"")
        );
        // No write grant to the read role, and no PASSWORD when none is configured.
        assert!(!sql.contains("INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA jurisearch_control, jurisearch_app TO \"jurisearch_read\""));
        assert!(!sql.to_uppercase().contains("PASSWORD"));
    }

    #[test]
    fn provision_sql_is_convergent_not_merely_additive() {
        let sql = build_provision_sql(&RoleSpec::default(), "jurisearch");
        // Attribute normalization brings a pre-existing over-privileged role back down (NOINHERIT too).
        assert!(sql.contains(
            "ALTER ROLE \"jurisearch_read\" LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS NOINHERIT;"
        ));
        // The legacy PUBLIC create surface is removed.
        assert!(sql.contains("REVOKE CREATE ON SCHEMA public FROM PUBLIC;"));
        // The read role is revoke-then-granted (converged), not merely topped up.
        assert!(sql.contains("REVOKE ALL ON ALL TABLES IN SCHEMA public, jurisearch_control, jurisearch_server, jurisearch_app FROM \"jurisearch_read\";"));
        // EVERY membership of the read login is dynamically revoked (not just the two app roles), so an
        // inherited / SET ROLE write path from an arbitrary pre-existing membership is closed.
        assert!(sql.contains("FROM pg_auth_members m"));
        assert!(sql.contains("EXECUTE format('REVOKE %I FROM %I', r.group_role, read_name);"));
        assert!(sql.contains("read_name text := 'jurisearch_read';"));
    }

    #[test]
    fn provision_sql_identifier_quotes_a_non_simple_owner_in_the_ownership_loop() {
        // A configured owner with a hyphen/uppercase must NOT be interpolated raw — the DO block
        // re-quotes it via `%I`, and the literal is bound to the `owner_name` variable.
        let spec = RoleSpec {
            owner_role: "Juris-Owner".to_owned(),
            ..RoleSpec::default()
        };
        let sql = build_provision_sql(&spec, "jurisearch");
        assert!(
            sql.contains("OWNER TO %I"),
            "ownership loop must re-quote the owner via %I"
        );
        assert!(
            sql.contains("owner_name text := 'Juris-Owner';"),
            "owner name must be bound as a literal, not interpolated into the command text"
        );
        // The raw (unquoted) owner name must never appear in an `OWNER TO <raw>` position.
        assert!(!sql.contains("OWNER TO Juris-Owner"), "{sql}");
    }

    #[test]
    fn provision_sql_emits_password_alter_when_configured() {
        let spec = RoleSpec {
            read_password: Some("r'p".to_owned()),
            ..RoleSpec::default()
        };
        let sql = build_provision_sql(&spec, "jurisearch");
        // Single quote in the password is doubled by the string-literal quoter.
        assert!(sql.contains("ALTER ROLE \"jurisearch_read\" PASSWORD 'r''p';"));
    }
}
