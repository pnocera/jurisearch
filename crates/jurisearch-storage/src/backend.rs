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

use crate::generations::{ActivationReadVisibility, REPLICATED_TABLES, SITE_PUBLIC_WRITE_TABLES};
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
    ///
    /// The session `search_path` is PINNED to `public` via the libpq `options=-c search_path=public`
    /// startup parameter (M1-B BLOCKER): the producer client-source contract ([`DbClientSource`]) runs the
    /// migrated psql helpers as *unqualified* SQL over this client, and on a shared external server a
    /// role-level `ALTER ROLE … SET search_path` default — or a schema named after the connecting role
    /// (the `"$user"` element of the stock `"$user", public` default) — would silently redirect every
    /// unqualified read/write off `public`. Pinning `public` ENFORCES the producer working schema instead
    /// of assuming it. This is behavior-preserving for the existing syncd / serve-site callers: the read
    /// snapshot path re-sets `SET LOCAL search_path` per read, the writer/activation path sets its own
    /// `SET search_path TO <generation>, public` where a generation schema is needed (otherwise it uses
    /// fully-qualified names), and no per-user schema is ever provisioned — so the stock default already
    /// resolved to `public` in every non-pathological deployment.
    pub fn connect(&self) -> Result<postgres::Client, StorageError> {
        let mut config = postgres::Config::new();
        config
            .host(&self.host)
            .port(self.port)
            .dbname(&self.dbname)
            .user(&self.user)
            .application_name(&self.application_name)
            .options(PIN_PUBLIC_SEARCH_PATH)
            .connect_timeout(Duration::from_secs(5));
        if let Some(password) = &self.password {
            config.password(password);
        }
        config
            .connect(postgres::NoTls)
            .map_err(StorageError::PostgresClient)
    }
}

/// libpq startup `options` that pin the session `search_path` to `public`. Applied to every client a
/// [`DbClientSource`] hands out (the contract boundary), so the producer's unqualified helper SQL can
/// never be redirected off `public` by a role-level default or a `"$user"`-named schema on a shared
/// external server (M1-B BLOCKER).
pub(crate) const PIN_PUBLIC_SEARCH_PATH: &str = "-c search_path=public";

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

/// S1 seam (work/10 M1-B) — a minimal **client FACTORY**: "hand me a FRESH, independent
/// `postgres::Client`". It is deliberately a connection *source*, not a single borrowed
/// `&mut postgres::Client`, because the producer build path opens SEVERAL independent clients from one
/// source (e.g. a main client AND a separate outbox-fence connection — see work/10 §2 S7). This is the
/// one abstraction that lets the producer run ingest → enrich → embed → package against an EXTERNAL
/// PostgreSQL ([`ConnectionConfig`]/[`WriterHandle`]) with the SAME code path that today only works
/// against the self-managed [`ManagedPostgres`]. Object-safe.
///
/// Each call MUST return a brand-new, independent client (callers may hold more than one at once); an
/// implementation must never hand back a shared/cached connection.
///
/// CONTRACT — the returned client's session `search_path` is PINNED to `public` (the producer working
/// schema). The producer runs the migrated psql helpers as *unqualified* SQL through this seam, so on a
/// shared external server it must not be redirected off `public` by a role-level `search_path` default or
/// a `"$user"`-named schema. Every impl below upholds this (the external [`ConnectionConfig`]/
/// [`WriterHandle`] path via [`ConnectionConfig::connect`], the self-managed path via
/// [`ManagedPostgres::client`]).
pub trait DbClientSource {
    /// Open a fresh, independent libpq client against the target database, with `search_path` pinned to
    /// `public` (see the trait contract).
    ///
    /// # Errors
    /// [`StorageError::PostgresClient`] if the connection fails.
    fn client(&self) -> Result<postgres::Client, StorageError>;
}

/// The self-managed producer/test path (`pg_ctl`-owned loopback PG) as a client source.
impl DbClientSource for ManagedPostgres {
    fn client(&self) -> Result<postgres::Client, StorageError> {
        ManagedPostgres::client(self)
    }
}

/// The external-PostgreSQL primitive as a client source: any role identity ([`ConnectionConfig`]) can
/// produce a client, so the producer can attach to an operator-run database with no managed server.
impl DbClientSource for ConnectionConfig {
    fn client(&self) -> Result<postgres::Client, StorageError> {
        self.connect()
    }
}

/// The external-PostgreSQL **writer** identity as a client source (the producer mutates the DB, so it
/// uses the writer role). Mirrors [`WriterHandle::client`].
impl DbClientSource for WriterHandle {
    fn client(&self) -> Result<postgres::Client, StorageError> {
        self.config.connect()
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

/// Which writer grant profile [`build_provision_sql`] issues. The owner and read roles are IDENTICAL
/// across profiles (read stays SELECT-only either way); only the **writer's `public` authority** differs,
/// because the two deployments give the writer a different real job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleProfile {
    /// SITE / client host (the syncd package applier). The writer READS the replicated `public`
    /// templates (to clone per-generation schemas) and writes ONLY the control/app namespaces plus the
    /// enumerated `public` accounting tables ([`SITE_PUBLIC_WRITE_TABLES`]). It NEVER writes the `public`
    /// WORKING tables — corpus data lives in per-generation schemas — so the site's read/write
    /// confidentiality split (serve queries split read vs write) is preserved.
    Site,
    /// EXTERNAL PRODUCER database (the box that BUILDS the corpus, provisioned by
    /// [`provision_producer_roles`] / [`crate::provision::provision_external_db`]). The writer is the
    /// BUILD PRINCIPAL: its unqualified helper SQL (`search_path=public`) writes the entire `public`
    /// working schema (`documents`, `chunks`, `graph_edges`, `official_api_responses`, dense/zone/citation,
    /// … plus all accounting), so it gets DML across the FULL `public` schema and `USAGE` on every public
    /// sequence — no enumerated table list to drift, and no query-confidentiality split (the producer
    /// authoritatively owns `public`).
    Producer,
}

/// Provision the least-privilege roles + grants for a **SITE / client** host on a freshly-migrated
/// database ([`RoleProfile::Site`]; idempotent; superuser connection). This is a **provisioning** step
/// (role names/passwords are deployment config, not a hardcoded schema migration). It:
///
/// 1. creates the NOLOGIN owner + LOGIN read/writer roles (idempotent), and makes the writer a member
///    of the owner so it can run the app's dynamic DDL without superuser;
/// 2. transfers ownership of the app-managed namespaces (`jurisearch_control` / `jurisearch_server` /
///    `jurisearch_app`) and the app-owned `public` objects to the owner, so the writer (member) can
///    `CREATE OR REPLACE` the stable views and create generation schemas later (2B);
/// 3. grants the **read** role SELECT-only on the control/manifest tables + the stable views (and a
///    default privilege so rebuilt views stay readable) — and **no** write anywhere;
/// 4. grants the **writer** role the DML + CREATE-on-database it needs to apply (the narrow site
///    `public` surface: SELECT on the replicated templates + DML on [`SITE_PUBLIC_WRITE_TABLES`]).
///
/// For the EXTERNAL PRODUCER database use [`provision_producer_roles`] instead — same owner/read model,
/// but the writer gets the FULL `public` working schema (see [`RoleProfile`]).
///
/// The per-generation grant (read role on each newly activated physical schema) is NOT here — it is an
/// activation postcondition (`activate_generation_with_guard_and_visibility`).
pub fn provision_roles(
    client: &mut postgres::Client,
    spec: &RoleSpec,
    dbname: &str,
) -> Result<(), StorageError> {
    let sql = build_provision_sql(spec, dbname, RoleProfile::Site);
    client
        .batch_execute(&sql)
        .map_err(StorageError::PostgresClient)
}

/// Provision the least-privilege roles + grants for the **EXTERNAL PRODUCER** database
/// ([`RoleProfile::Producer`]; idempotent; superuser connection). Identical to [`provision_roles`] for
/// the owner/read roles, but the **writer** is the corpus BUILD PRINCIPAL, so it receives DML across the
/// FULL `public` working schema + `USAGE` on every public sequence (and default privileges so
/// later-created public objects stay covered) — not the site's narrow enumerated surface. See
/// [`RoleProfile::Producer`].
pub fn provision_producer_roles(
    client: &mut postgres::Client,
    spec: &RoleSpec,
    dbname: &str,
) -> Result<(), StorageError> {
    let sql = build_provision_sql(spec, dbname, RoleProfile::Producer);
    client
        .batch_execute(&sql)
        .map_err(StorageError::PostgresClient)
}

/// Build the **convergent**, idempotent provisioning DDL. Role names are identifier-quoted; the
/// `pg_roles` existence checks / passwords / the dynamic owner are string-literal-quoted (and re-quoted
/// as identifiers at runtime with `%I`). It does not merely *add* privileges — it normalizes role
/// attributes and revokes accidental surfaces first, so an over-privileged pre-existing role (or a
/// legacy `PUBLIC` CREATE on `public`) is brought back down to least privilege, not left in place.
fn build_provision_sql(spec: &RoleSpec, dbname: &str, profile: RoleProfile) -> String {
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
    //    The control/app surface is IDENTICAL across profiles; only the `public` authority differs.
    sql.push_str(&format!(
        "GRANT USAGE ON SCHEMA public, jurisearch_control, jurisearch_server, jurisearch_app TO {writer};\n"
    ));
    sql.push_str(&format!(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA jurisearch_control, jurisearch_app TO {writer};\n"
    ));
    sql.push_str(&format!(
        "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA jurisearch_control, jurisearch_app TO {writer};\n"
    ));
    // CONVERGE the writer's WHOLE `public` table AND sequence surface FIRST (both profiles), so a
    // deployment that previously received a broad `public` table grant OR a broad
    // `GRANT … ON ALL SEQUENCES IN SCHEMA public` (drift) is brought back to exactly what the profile
    // needs — additive grants alone would leave an over-privileged writer in place (codex r2 WARN).
    sql.push_str(&format!(
        "REVOKE ALL ON ALL TABLES IN SCHEMA public FROM {writer};\n"
    ));
    sql.push_str(&format!(
        "REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM {writer};\n"
    ));
    // CONVERGE the writer's FUTURE-object (DEFAULT) `public` privileges too (both profiles). The
    // current-object revokes above only touch tables/sequences that exist NOW; a prior run or operator
    // drift may have left a broad `ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA public GRANT ALL ON
    // TABLES/SEQUENCES TO {writer}`, so every OWNER-created public object would silently inherit
    // `TRUNCATE`/`REFERENCES`/`TRIGGER` (tables) or `UPDATE` (sequences). Revoke-first the defaults, then
    // each profile re-grants ONLY what it intends: the producer re-grants the narrow DML/USAGE default
    // (below); the SITE profile re-grants NOTHING, so the site writer keeps NO public default privileges.
    // `REVOKE ALL` is idempotent — a no-op when no default privilege exists (codex r3 WARN).
    sql.push_str(&format!(
        "ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA public REVOKE ALL ON TABLES FROM {writer};\n"
    ));
    sql.push_str(&format!(
        "ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA public REVOKE ALL ON SEQUENCES FROM {writer};\n"
    ));

    match profile {
        RoleProfile::Site => {
            // SITE / client applier: it creates each generation's tables as
            // `CREATE TABLE … (LIKE public.<table> …)`, so it needs SELECT on exactly the `public`
            // REPLICATED templates — never write, and never an unrelated `public` table. Corpus data is
            // written into the per-generation schemas, so `public` working tables stay untouched here.
            let replicated = REPLICATED_TABLES
                .iter()
                .map(|table| format!("public.{}", sql_identifier(table)))
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(&format!("GRANT SELECT ON {replicated} TO {writer};\n"));
            // The site writer's narrow `public` DML surface is the SHARED CONSTANT
            // (`SITE_PUBLIC_WRITE_TABLES`), so the grants can never drift from the schema: it records
            // ingest accounting, the package catalog/change-log, the manifest, and the version stamps —
            // all in `public`. Without this the postcondition could report `writer_can_write = true` (an
            // `index_manifest` upsert) while the applier fails at its first accounting write.
            let site_write = SITE_PUBLIC_WRITE_TABLES
                .iter()
                .map(|table| format!("public.{}", sql_identifier(table)))
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(&format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON {site_write} TO {writer};\n"
            ));
            // Those tables include `bigserial` columns (`ingest_member.member_id`,
            // `ingest_error.error_id`, `package_change_log.change_seq`), so the writer also needs USAGE on
            // their backing sequences or the first `INSERT` fails with `permission denied for sequence`.
            // Grant ONLY the sequences OWNED BY the site-write tables (via `pg_depend`, `deptype = 'a'`),
            // so this stays least-privilege: a replicated table's `public` sequence (e.g.
            // `official_api_responses.response_id`) is NOT granted. The `pg_depend` set is constrained to
            // a `public` sequence OWNED BY a `public` table (both namespaces pinned), so it can never pick
            // up a same-named table in another schema. The converge-revoke above already dropped any prior
            // broad sequence grant.
            let site_write_literals = SITE_PUBLIC_WRITE_TABLES
                .iter()
                .map(|table| sql_string_literal(table))
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(&format!(
                "DO $do$ DECLARE seq text; writer_name text := {writer_lit}; BEGIN \
                   FOR seq IN \
                     SELECT quote_ident(n.nspname) || '.' || quote_ident(s.relname) \
                     FROM pg_class s \
                     JOIN pg_namespace n ON n.oid = s.relnamespace \
                     JOIN pg_depend d ON d.objid = s.oid AND d.deptype = 'a' \
                     JOIN pg_class t ON t.oid = d.refobjid \
                     JOIN pg_namespace tn ON tn.oid = t.relnamespace \
                     WHERE s.relkind = 'S' AND n.nspname = 'public' AND tn.nspname = 'public' \
                       AND t.relname IN ({site_write_literals}) LOOP \
                     EXECUTE format('GRANT USAGE, SELECT ON SEQUENCE %s TO %I', seq, writer_name); \
                   END LOOP; \
                 END $do$;\n",
                writer_lit = sql_string_literal(&spec.writer_role),
            ));
        }
        RoleProfile::Producer => {
            // EXTERNAL PRODUCER: the writer is the corpus BUILD PRINCIPAL. Its helper SQL runs unqualified
            // under `search_path=public` and mutates the entire `public` working schema (documents,
            // chunks, graph_edges, official_api_responses, dense/zone/citation, … and all accounting), so
            // grant DML across the FULL `public` schema and USAGE on EVERY public sequence (e.g. the
            // `official_api_responses.response_id` bigserial). No enumerated table list — drift-proof.
            sql.push_str(&format!(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO {writer};\n"
            ));
            sql.push_str(&format!(
                "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO {writer};\n"
            ));
            // Tables/sequences the OWNER creates LATER (re-baseline DDL, owner-replayed migrations) stay
            // covered without a re-provision. (The primary drift-proofing is the idempotent re-run of this
            // provisioner, whose `… ON ALL TABLES/SEQUENCES` re-grants after any new migration.)
            sql.push_str(&format!(
                "ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA public \
                 GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO {writer};\n"
            ));
            sql.push_str(&format!(
                "ALTER DEFAULT PRIVILEGES FOR ROLE {owner} IN SCHEMA public \
                 GRANT USAGE, SELECT ON SEQUENCES TO {writer};\n"
            ));
        }
    }

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

    /// Type-level proof (no live PG) that the S1 [`DbClientSource`] client-factory seam is implemented
    /// for the self-managed [`ManagedPostgres`] AND the external-PG identities ([`ConnectionConfig`] /
    /// [`WriterHandle`]), and that it is object-safe — i.e. an external PostgreSQL satisfies the same
    /// trait the producer needs, with no `ManagedPostgres`. This is the compile-time half of the M1-B
    /// S1 deliverable; the run-time equivalence is exercised by `tests/client_source_parity.rs` (live PG).
    #[test]
    fn db_client_source_is_implemented_for_managed_and_external_and_is_object_safe() {
        fn assert_impl<T: DbClientSource>() {}
        assert_impl::<ManagedPostgres>();
        assert_impl::<ConnectionConfig>();
        assert_impl::<WriterHandle>();
        // Object safety: a `&dyn DbClientSource` must be a valid type.
        let _: Option<&dyn DbClientSource> = None;
    }

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
        let sql = build_provision_sql(&RoleSpec::default(), "jurisearch", RoleProfile::Site);
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
    fn site_profile_grants_writer_the_enumerated_public_write_surface_and_owned_sequences() {
        let sql = build_provision_sql(&RoleSpec::default(), "jurisearch", RoleProfile::Site);
        // Every site-writable `public` table is in the writer's explicit DML grant — including the
        // accounting tables a freshly provisioned applier must write at its first run.
        for table in SITE_PUBLIC_WRITE_TABLES {
            assert!(
                sql.contains(&format!("public.\"{table}\"")),
                "writer DML grant is missing `public.{table}`:\n{sql}"
            );
        }
        for table in ["ingest_run", "ingest_member", "ingest_error"] {
            assert!(sql.contains(&format!("public.\"{table}\"")), "{table}");
        }
        // The SITE profile must NOT grant DML on every public table (that is the producer's authority).
        assert!(
            !sql.contains("GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public"),
            "site writer must not get full public DML: {sql}"
        );
        // USAGE on the bigserial sequences OWNED BY the site-write tables (pg_depend, deptype 'a'),
        // scoped to those tables only — NOT `ALL SEQUENCES IN SCHEMA public` (which would leak a
        // replicated table's sequence to the writer).
        assert!(sql.contains("d.deptype = 'a'"), "{sql}");
        assert!(
            sql.contains("GRANT USAGE, SELECT ON SEQUENCE %s TO %I"),
            "{sql}"
        );
        assert!(
            !sql.contains("GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public"),
            "must not grant the site writer USAGE on every public sequence: {sql}"
        );
        // The pg_depend scan is pinned to a public sequence OWNED BY a public table (both namespaces).
        assert!(sql.contains("tn.nspname = 'public'"), "{sql}");
        assert!(sql.contains("t.relname IN ('index_manifest'"), "{sql}");
        assert!(sql.contains("'ingest_member'"), "{sql}");
        // The public sequence surface is CONVERGED (revoked) first, like the table surface — so a
        // previously over-granted writer cannot keep broad public sequence access (codex r2 WARN).
        assert!(
            sql.contains("REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM \"jurisearch_write\";"),
            "site writer public sequence surface must be converge-revoked: {sql}"
        );
        // FUTURE-object (DEFAULT) public privileges are converge-revoked too, and the SITE profile
        // re-grants NOTHING as default — so the site writer keeps NO public default privileges (codex
        // r3 WARN).
        assert!(
            sql.contains(
                "ALTER DEFAULT PRIVILEGES FOR ROLE \"jurisearch_owner\" IN SCHEMA public REVOKE ALL ON TABLES FROM \"jurisearch_write\";"
            ),
            "site writer default table privileges must be converge-revoked: {sql}"
        );
        assert!(
            sql.contains(
                "ALTER DEFAULT PRIVILEGES FOR ROLE \"jurisearch_owner\" IN SCHEMA public REVOKE ALL ON SEQUENCES FROM \"jurisearch_write\";"
            ),
            "site writer default sequence privileges must be converge-revoked: {sql}"
        );
        assert!(
            !sql.contains(
                "ALTER DEFAULT PRIVILEGES FOR ROLE \"jurisearch_owner\" IN SCHEMA public GRANT"
            ),
            "site profile must leave NO public default privileges for the writer: {sql}"
        );
    }

    #[test]
    fn producer_profile_grants_writer_the_full_public_working_schema_and_all_sequences() {
        let sql = build_provision_sql(&RoleSpec::default(), "jurisearch", RoleProfile::Producer);
        // The producer writer is the BUILD PRINCIPAL: DML across the FULL public schema + USAGE on every
        // public sequence (so unqualified writes to documents/chunks/official_api_responses/… succeed).
        assert!(
            sql.contains(
                "GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO \"jurisearch_write\";"
            ),
            "producer writer must get full public DML: {sql}"
        );
        assert!(
            sql.contains(
                "GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO \"jurisearch_write\";"
            ),
            "producer writer must get USAGE on all public sequences: {sql}"
        );
        // Converge-revoke before the additive grants (no drift), for BOTH tables and sequences.
        assert!(
            sql.contains("REVOKE ALL ON ALL TABLES IN SCHEMA public FROM \"jurisearch_write\";")
        );
        assert!(
            sql.contains("REVOKE ALL ON ALL SEQUENCES IN SCHEMA public FROM \"jurisearch_write\";")
        );
        // Default privileges are ALSO converge-revoked first, so a drifted broad `GRANT ALL` default
        // cannot leave TRUNCATE/REFERENCES/TRIGGER (tables) or UPDATE (sequences) on future objects
        // before the narrow re-grant (codex r3 WARN).
        assert!(
            sql.contains(
                "ALTER DEFAULT PRIVILEGES FOR ROLE \"jurisearch_owner\" IN SCHEMA public REVOKE ALL ON TABLES FROM \"jurisearch_write\";"
            ),
            "producer default table privileges must be converge-revoked: {sql}"
        );
        assert!(
            sql.contains(
                "ALTER DEFAULT PRIVILEGES FOR ROLE \"jurisearch_owner\" IN SCHEMA public REVOKE ALL ON SEQUENCES FROM \"jurisearch_write\";"
            ),
            "producer default sequence privileges must be converge-revoked: {sql}"
        );
        // The revoke-first precedes the narrow default re-grant.
        let revoke_default_tables = sql
            .find("ALTER DEFAULT PRIVILEGES FOR ROLE \"jurisearch_owner\" IN SCHEMA public REVOKE ALL ON TABLES")
            .expect("default-table converge-revoke present");
        let grant_default_tables = sql
            .find(
                "ALTER DEFAULT PRIVILEGES FOR ROLE \"jurisearch_owner\" IN SCHEMA public \
                 GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES",
            )
            .expect("default-table re-grant present");
        assert!(
            revoke_default_tables < grant_default_tables,
            "default-privilege revoke must precede the re-grant: {sql}"
        );
        // Default privileges so later owner-created public objects stay covered (drift-proof).
        assert!(
            sql.contains(
                "ALTER DEFAULT PRIVILEGES FOR ROLE \"jurisearch_owner\" IN SCHEMA public \
                 GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO \"jurisearch_write\";"
            ),
            "{sql}"
        );
        assert!(
            sql.contains(
                "ALTER DEFAULT PRIVILEGES FOR ROLE \"jurisearch_owner\" IN SCHEMA public \
                 GRANT USAGE, SELECT ON SEQUENCES TO \"jurisearch_write\";"
            ),
            "{sql}"
        );
        // The producer profile must NOT fall back to the enumerated-table / pg_depend site model.
        assert!(
            !sql.contains("d.deptype = 'a'"),
            "producer profile must not use the enumerated owned-sequence pg_depend block: {sql}"
        );
        // The read role stays SELECT-only even on the producer DB (no public DML for read).
        assert!(
            !sql.contains("ON ALL TABLES IN SCHEMA public TO \"jurisearch_read\""),
            "read role must never get public DML: {sql}"
        );
    }

    #[test]
    fn provision_sql_is_convergent_not_merely_additive() {
        let sql = build_provision_sql(&RoleSpec::default(), "jurisearch", RoleProfile::Site);
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
        let sql = build_provision_sql(&spec, "jurisearch", RoleProfile::Site);
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
        let sql = build_provision_sql(&spec, "jurisearch", RoleProfile::Site);
        // Single quote in the password is doubled by the string-literal quoter.
        assert!(sql.contains("ALTER ROLE \"jurisearch_read\" PASSWORD 'r''p';"));
    }
}
