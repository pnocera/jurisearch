//! Client storage topology: per-corpus physical generations + stable views + the control cursor
//! (migration v20; design §4, §7.2, §7.4; plan P2).
//!
//! The CLIENT serves replicated data from per-corpus **physical generation schemas**
//! `jurisearch_server_<corpus>_gNNNN` fronted by stable `jurisearch_server` views, with the apply
//! cursor and generation registry in `jurisearch_control` (never swapped) and the app's tables in
//! `jurisearch_app` (preserved across every re-baseline). A re-baseline is a **view repoint**
//! (`CREATE OR REPLACE VIEW`), not a destructive `DROP SCHEMA` on the operated path (§7.4).
//!
//! Two read modes (codex P2 architecture, see memory):
//! * **Hot indexed retrieval** (search/zone) must target the **qualified physical** generation tables
//!   — BM25 (`pg_search`) and IVFFlat indexes live there; a view cannot be index-scanned. Use
//!   [`active_generation_schema`] to resolve `corpus → active physical schema`.
//! * **Non-indexed reads** (fetch, stats, context, compatibility) read the stable views; a client read
//!   connection may `SET search_path = jurisearch_server, public` so unqualified SQL resolves to them.
//!
//! **DDL classification** (the key P2 boundary): only the replicated *data* tables
//! ([`REPLICATED_TABLES`]) are emitted per-generation; `jurisearch_control`/`jurisearch_app`/the
//! `jurisearch_server` views and the global `package_change_log`/`index_manifest`/`schema_migrations`/
//! `ingest_*` are never per-generation. The PRODUCER keeps its authoritative working set in `public`.

use crate::runtime::{ManagedPostgres, StorageError, sql_identifier, sql_string_literal};
use postgres::GenericClient;

/// Session/xact advisory-lock key for the package-apply + generation switch (design §7.3/§7.4: one
/// apply at a time, fail-clean rather than block behind a long reader).
pub const APPLY_ADVISORY_LOCK_KEY: i64 = 0x6a75_7269_7331; // "juris1"

/// The stable namespace holding one view per replicated relation.
pub const SERVER_VIEW_SCHEMA: &str = "jurisearch_server";
/// The control namespace (cursor + registry), never swapped.
pub const CONTROL_SCHEMA: &str = "jurisearch_control";
/// The app-writable namespace, preserved across re-baselines.
pub const APP_SCHEMA: &str = "jurisearch_app";

/// The design §4.2 replicated *data* tables — the per-generation set. Each is cloned into a
/// generation schema and fronted by a `jurisearch_server` view. (Control/operational tables are
/// intentionally absent; [`is_replicated_table`] is the single classifier.)
pub const REPLICATED_TABLES: &[&str] = &[
    "documents",
    "chunks",
    "chunk_embeddings",
    "graph_edges",
    "legi_metadata_roots",
    "zone_units",
    "zone_unit_embeddings",
    "decision_zones",
    "decision_legislation_citations",
    "legislation_citation_resolutions",
    "official_api_responses",
];

/// Global/control/operational tables that are **never** emitted per-generation (the other half of the
/// DDL classification). Asserted disjoint from [`REPLICATED_TABLES`] by a test.
pub const NON_GENERATION_TABLES: &[&str] = &[
    "index_manifest",
    "schema_migrations",
    "package_change_log",
    "ingest_run",
    "ingest_member",
    "ingest_error",
];

/// Whether `table_name` is a replicated data table (emitted per-generation + fronted by a view).
#[must_use]
pub fn is_replicated_table(table_name: &str) -> bool {
    REPLICATED_TABLES.contains(&table_name)
}

/// The logical generation name for a corpus + counter, e.g. `core_g0001` (design §4.1).
#[must_use]
pub fn generation_name(corpus: &str, counter: u32) -> String {
    format!("{corpus}_g{counter:04}")
}

/// The physical schema name backing a generation, e.g. `jurisearch_server_core_g0001`.
#[must_use]
pub fn generation_schema(corpus: &str, counter: u32) -> String {
    format!("{SERVER_VIEW_SCHEMA}_{}", generation_name(corpus, counter))
}

/// The physical schema name for an already-formed generation label (`core_g0001`).
#[must_use]
pub fn schema_for_generation(generation: &str) -> String {
    format!("{SERVER_VIEW_SCHEMA}_{generation}")
}

/// Create the physical schema for a new generation and clone the replicated tables into it
/// (`LIKE public.<t> INCLUDING ALL` — columns, defaults, CHECKs, generated columns, and the BM25 /
/// IVFFlat index *definitions*), then register the generation as `building` (design §7.2). The
/// caller loads rows and (P3) validates before activating.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn create_generation_schema<C: GenericClient>(
    client: &mut C,
    corpus: &str,
    counter: u32,
    source_baseline_id: Option<&str>,
) -> Result<String, StorageError> {
    let generation = generation_name(corpus, counter);
    let schema = generation_schema(corpus, counter);

    // Generation names are single-use: register the row FIRST (the PK fails loudly on a re-create,
    // so a half-built generation is never silently reused — retry goes through an explicit cleanup).
    client
        .execute(
            "INSERT INTO jurisearch_control.generation_registry \
                 (corpus, generation, physical_schema, state, source_baseline_id) \
             VALUES ($1, $2, $3, 'building', $4);",
            &[&corpus, &generation, &schema, &source_baseline_id],
        )
        .map_err(StorageError::PostgresClient)?;

    // CREATE (not IF NOT EXISTS): a pre-existing schema for a fresh generation is an error.
    let mut ddl = format!("CREATE SCHEMA {};\n", sql_identifier(&schema));
    for table in REPLICATED_TABLES {
        // INCLUDING ALL clones the index definitions too (BM25/IVFFlat), so the generation is
        // self-contained; the client (P3) (re)builds/finalises them after load per §9.3.
        ddl.push_str(&format!(
            "CREATE TABLE {}.{} (LIKE public.{} INCLUDING ALL);\n",
            sql_identifier(&schema),
            sql_identifier(table),
            sql_identifier(table),
        ));
    }
    client
        .batch_execute(&ddl)
        .map_err(StorageError::PostgresClient)?;
    Ok(generation)
}

/// Create the physical schema for a new generation cloning **column shape only**
/// (`LIKE public.<t> INCLUDING ALL EXCLUDING INDEXES` — columns, defaults, generated columns, CHECK /
/// NOT-NULL constraints, but **no** indexes and **no** PK/UNIQUE, which are index-backed), then
/// register the generation as `building` (plan P3 D3). This is the **baseline load** path: bulk-load
/// rows into unindexed tables, then [`build_generation_indexes`] recreates PK/UNIQUE + every index and
/// builds the IVFFlat ANN at corpus-sized `lists` (§9.3, INV-6) before activation. (Contrast
/// [`create_generation_schema`], which clones INCLUDING ALL for the P2 topology tests.)
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn create_generation_load_tables<C: GenericClient>(
    client: &mut C,
    corpus: &str,
    counter: u32,
    source_baseline_id: Option<&str>,
) -> Result<String, StorageError> {
    let generation = generation_name(corpus, counter);
    let schema = generation_schema(corpus, counter);

    // Single-use registry row FIRST (PK fails loudly on a re-create), as in `create_generation_schema`.
    client
        .execute(
            "INSERT INTO jurisearch_control.generation_registry \
                 (corpus, generation, physical_schema, state, source_baseline_id) \
             VALUES ($1, $2, $3, 'building', $4);",
            &[&corpus, &generation, &schema, &source_baseline_id],
        )
        .map_err(StorageError::PostgresClient)?;

    let mut ddl = format!("CREATE SCHEMA {};\n", sql_identifier(&schema));
    for table in REPLICATED_TABLES {
        // EXCLUDING INDEXES so the bulk load goes into unindexed tables (fast, and the IVFFlat `lists`
        // are chosen at build time over the loaded rows). This also drops PK/UNIQUE (index-backed) —
        // `build_generation_indexes` recreates them after load, so the generation ends structurally
        // equal to `public` before it is ever activated.
        ddl.push_str(&format!(
            "CREATE TABLE {}.{} (LIKE public.{} INCLUDING ALL EXCLUDING INDEXES);\n",
            sql_identifier(&schema),
            sql_identifier(table),
            sql_identifier(table),
        ));
    }
    client
        .batch_execute(&ddl)
        .map_err(StorageError::PostgresClient)?;
    Ok(generation)
}

/// Per-table index/constraint build report for a generation (plan P3 D3/D4 — used to prove inventory
/// + recorded on the activation record).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationIndexReport {
    /// PK/UNIQUE constraints recreated (schema-qualified).
    pub constraints_built: u32,
    /// FOREIGN KEY constraints recreated, retargeted within the generation (plan P3 WARN-2).
    pub foreign_keys_built: u32,
    /// Non-IVFFlat indexes replayed from `public` (btree/GIN/BM25).
    pub indexes_built: u32,
    /// IVFFlat ANN indexes built at corpus-sized `lists`, with the chosen `lists` per index.
    pub ivfflat_built: Vec<(String, u32)>,
}

/// After a baseline bulk-load into a [`create_generation_load_tables`] generation, rebuild the full
/// index/constraint inventory **inside the generation** before activation (plan P3 D3/D4, §9.3,
/// INV-6): recreate PK/UNIQUE and every non-IVFFlat index by replaying the producer's definitions
/// (`pg_get_constraintdef`/`pg_get_indexdef`, rewritten to the generation schema), then build the
/// IVFFlat ANN indexes at `recommended_ivfflat_lists(rowcount)`, then `ANALYZE`. Index/constraint
/// names are kept as-is — they are schema-scoped, so two generations never collide.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn build_generation_indexes<C: GenericClient>(
    client: &mut C,
    generation: &str,
    corpus: &str,
) -> Result<GenerationIndexReport, StorageError> {
    let schema = schema_for_generation(generation);
    let _ = corpus; // corpus scope is already materialised in the generation; kept for symmetry/logging.
    let schema_ident = sql_identifier(&schema);
    // Targets below are schema-explicit, but replayed index expressions reference functions/operator
    // classes that live in `public` (`jurisearch_normalized_case_numbers`, pgvector `vector_l2_ops`,
    // pg_search BM25 operators) unqualified — keep `public` on the path so they resolve.
    client
        .batch_execute(&format!("SET search_path TO {schema_ident}, public;"))
        .map_err(StorageError::PostgresClient)?;
    let mut report = GenerationIndexReport {
        constraints_built: 0,
        foreign_keys_built: 0,
        indexes_built: 0,
        ivfflat_built: Vec::new(),
    };

    for table in REPLICATED_TABLES {
        let public_regclass = sql_string_literal(&format!("public.{table}"));
        let table_ident = sql_identifier(table);

        // 1. Recreate PK/UNIQUE constraints (index-backed, so EXCLUDING INDEXES dropped them).
        let constraints = client
            .query(
                &format!(
                    "SELECT conname, pg_get_constraintdef(c.oid) AS def \
                     FROM pg_constraint c \
                     WHERE c.conrelid = {public_regclass}::regclass AND c.contype IN ('p','u') \
                     ORDER BY conname;"
                ),
                &[],
            )
            .map_err(StorageError::PostgresClient)?;
        for row in &constraints {
            let conname: String = row.get("conname");
            let def: String = row.get("def");
            client
                .batch_execute(&format!(
                    "ALTER TABLE {schema_ident}.{table_ident} ADD CONSTRAINT {} {def};",
                    sql_identifier(&conname)
                ))
                .map_err(StorageError::PostgresClient)?;
            report.constraints_built += 1;
        }

        // 2. Replay non-IVFFlat, non-constraint-backing indexes (btree/GIN/BM25) — rewrite the
        //    `public.` qualifier to the generation schema. IVFFlat is built separately at step 3.
        let indexes = client
            .query(
                &format!(
                    "SELECT pg_get_indexdef(x.indexrelid) AS def \
                     FROM pg_index x \
                     JOIN pg_class i ON i.oid = x.indexrelid \
                     JOIN pg_am am ON am.oid = i.relam \
                     WHERE x.indrelid = {public_regclass}::regclass \
                       AND NOT x.indisprimary AND NOT x.indisunique \
                       AND am.amname <> 'ivfflat' \
                     ORDER BY i.relname;"
                ),
                &[],
            )
            .map_err(StorageError::PostgresClient)?;
        for row in &indexes {
            let def: String = row.get("def");
            // `pg_get_indexdef` emits the table reference unquoted (`ON public.<table>`) and, with
            // `public` on the search_path, leaves function refs in the expression unqualified — so the
            // only `public.` is the `ON` target. Retarget JUST that to the generation schema (the index
            // name stays — it is schema-scoped, so it cannot collide with `public`'s same-named index).
            let retargeted = def.replacen(
                &format!("ON public.{table}"),
                &format!("ON {schema_ident}.{table_ident}"),
                1,
            );
            client
                .batch_execute(&format!("{retargeted};"))
                .map_err(StorageError::PostgresClient)?;
            report.indexes_built += 1;
        }
    }

    // 2b. Recreate FOREIGN KEY constraints (plan P3 WARN-2) — a SECOND pass, now that every table has
    //     its PK/UNIQUE. Built structurally from `pg_constraint` (not string-rewritten) and retargeted:
    //     a reference to a replicated table points INTO the generation (self-contained); a reference to
    //     a non-replicated table is the documented cross-schema exception and stays on `public`.
    for table in REPLICATED_TABLES {
        let public_regclass = sql_string_literal(&format!("public.{table}"));
        let table_ident = sql_identifier(table);
        let fks = client
            .query(
                &format!(
                    "SELECT c.conname, \
                            (SELECT string_agg(quote_ident(a.attname), ', ' ORDER BY k.ord) \
                             FROM unnest(c.conkey) WITH ORDINALITY AS k(attnum, ord) \
                             JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = k.attnum) \
                                AS local_cols, \
                            cf.relname AS ref_table, \
                            (SELECT string_agg(quote_ident(a.attname), ', ' ORDER BY k.ord) \
                             FROM unnest(c.confkey) WITH ORDINALITY AS k(attnum, ord) \
                             JOIN pg_attribute a ON a.attrelid = c.confrelid AND a.attnum = k.attnum) \
                                AS ref_cols, \
                            c.confdeltype, c.confupdtype \
                     FROM pg_constraint c \
                     JOIN pg_class cf ON cf.oid = c.confrelid \
                     WHERE c.conrelid = {public_regclass}::regclass AND c.contype = 'f' \
                     ORDER BY c.conname;"
                ),
                &[],
            )
            .map_err(StorageError::PostgresClient)?;
        for row in &fks {
            let conname: String = row.get("conname");
            let local_cols: String = row.get("local_cols");
            let ref_table: String = row.get("ref_table");
            let ref_cols: String = row.get("ref_cols");
            let del: i8 = row.get("confdeltype");
            let upd: i8 = row.get("confupdtype");
            // Replicated reference → into the generation; otherwise keep `public` (documented exception).
            let ref_schema = if REPLICATED_TABLES.contains(&ref_table.as_str()) {
                schema_ident.clone()
            } else {
                sql_identifier("public")
            };
            client
                .batch_execute(&format!(
                    "ALTER TABLE {schema_ident}.{table_ident} ADD CONSTRAINT {con} \
                     FOREIGN KEY ({local_cols}) REFERENCES {ref_schema}.{ref} ({ref_cols}) \
                     ON DELETE {on_delete} ON UPDATE {on_update};",
                    con = sql_identifier(&conname),
                    ref = sql_identifier(&ref_table),
                    on_delete = fk_action(del),
                    on_update = fk_action(upd),
                ))
                .map_err(StorageError::PostgresClient)?;
            report.foreign_keys_built += 1;
        }
    }

    // 3. Build the IVFFlat ANN indexes at corpus-sized `lists` over the LOADED rows (§9.3). The two
    //    dense indexes live on `chunk_embeddings.embedding` and `zone_unit_embeddings.embedding`.
    for (table, index_name) in [
        ("chunk_embeddings", crate::dense::DENSE_VECTOR_INDEX_NAME),
        (
            "zone_unit_embeddings",
            crate::zone_units::ZONE_UNIT_VECTOR_INDEX_NAME,
        ),
    ] {
        let table_ident = sql_identifier(table);
        let count: i64 = client
            .query_one(
                &format!("SELECT count(*)::bigint FROM {schema_ident}.{table_ident};"),
                &[],
            )
            .map_err(StorageError::PostgresClient)?
            .get(0);
        let lists = crate::dense::recommended_ivfflat_lists(count);
        client
            .batch_execute(&format!(
                "CREATE INDEX {idx} ON {schema_ident}.{table_ident} \
                 USING ivfflat (embedding vector_l2_ops) WITH (lists = {lists});",
                idx = sql_identifier(index_name),
            ))
            .map_err(StorageError::PostgresClient)?;
        report.ivfflat_built.push((index_name.to_owned(), lists));
    }

    // 4. ANALYZE every generation table so the planner has stats before the corpus goes live.
    for table in REPLICATED_TABLES {
        client
            .batch_execute(&format!(
                "ANALYZE {schema_ident}.{};",
                sql_identifier(table)
            ))
            .map_err(StorageError::PostgresClient)?;
    }
    Ok(report)
}

/// Map a `pg_constraint.confdeltype`/`confupdtype` byte to its SQL referential action.
fn fk_action(code: i8) -> &'static str {
    match u8::from_ne_bytes(code.to_ne_bytes()) {
        b'r' => "RESTRICT",
        b'c' => "CASCADE",
        b'n' => "SET NULL",
        b'd' => "SET DEFAULT",
        _ => "NO ACTION", // 'a'
    }
}

/// Write the `index_manifest` row the dense query path reads for the corpus-sized `default_probes`
/// (plan P3 WARN-2; mirrors what `dense.rs`/`zone_units.rs` finalize write on the producer). `key` is
/// `embedding` (chunks) or `zone_embedding` (zone units). Coverage counts come from the generation, so
/// the manifest reflects exactly what was loaded + indexed before activation. The `default_probes` is
/// the package-declared value, so a fresh client honours the producer's probe tuning instead of a
/// hard-coded fallback.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
#[allow(clippy::too_many_arguments)]
pub fn upsert_generation_dense_manifest<C: GenericClient>(
    client: &mut C,
    schema: &str,
    key: &str,
    parent_table: &str,
    embedding_table: &str,
    index_name: &str,
    lists: u32,
    default_probes: u32,
    embedding_fingerprint: &str,
    model: &str,
    dimension: i32,
    normalize: bool,
) -> Result<(), StorageError> {
    let schema_ident = sql_identifier(schema);
    let parents: i64 = client
        .query_one(
            &format!(
                "SELECT count(*)::bigint FROM {schema_ident}.{};",
                sql_identifier(parent_table)
            ),
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    let embeddings: i64 = client
        .query_one(
            &format!(
                "SELECT count(*)::bigint FROM {schema_ident}.{};",
                sql_identifier(embedding_table)
            ),
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    let value = serde_json::json!({
        "embedding_fingerprint": embedding_fingerprint,
        "model": model,
        "dimension": dimension,
        "normalize": normalize,
        "vector_index": {
            "name": index_name,
            "method": "ivfflat",
            "operator_class": "vector_l2_ops",
            "lists": lists,
            "default_probes": default_probes,
        },
        "coverage": { "parents": parents, "embeddings": embeddings },
    })
    .to_string();
    client
        .execute(
            "INSERT INTO index_manifest(key, value, updated_at) \
             VALUES ($1, $2::text::jsonb, now()) \
             ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at;",
            &[&key, &value],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// The next per-corpus generation counter to allocate (1 + the highest existing counter for `corpus`
/// in `generation_registry`, or 1 if none) — so a re-apply after a failed/retired generation never
/// reuses a name (plan P3 D7). Generation names are `<corpus>_g<NNNN>`; the counter is parsed back out.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn next_generation_counter<C: GenericClient>(
    client: &mut C,
    corpus: &str,
) -> Result<u32, StorageError> {
    let prefix = format!("{corpus}_g");
    let rows = client
        .query(
            "SELECT generation FROM jurisearch_control.generation_registry WHERE corpus = $1;",
            &[&corpus],
        )
        .map_err(StorageError::PostgresClient)?;
    let highest = rows
        .iter()
        .filter_map(|row| {
            row.get::<_, String>("generation")
                .strip_prefix(&prefix)
                .and_then(|n| n.parse::<u32>().ok())
        })
        .max()
        .unwrap_or(0);
    Ok(highest + 1)
}

/// The non-generated columns of a replicated `public` table, in ordinal order — the explicit column
/// list used for both COPY-out (producer) and COPY-in (client), and recorded per file in the manifest
/// so producer and consumer move the **same** columns in the **same** order (plan P3 D2). Generated
/// columns (e.g. `documents.corpus`) are excluded — they cannot be inserted and are recomputed by the
/// clone's own definition.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn replicated_table_columns<C: GenericClient>(
    client: &mut C,
    table: &str,
) -> Result<Vec<String>, StorageError> {
    let rows = client
        .query(
            "SELECT column_name FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = $1 AND is_generated = 'NEVER' \
             ORDER BY ordinal_position;",
            &[&table],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(rows
        .iter()
        .map(|row| row.get::<_, String>("column_name"))
        .collect())
}

/// The `SELECT` body (with an explicit, generated-column-free column list and the corpus scope) that
/// the producer wraps in `COPY (<select>) TO STDOUT (FORMAT binary)` to export one replicated table's
/// rows for a baseline (plan P3). Mirrors [`populate_generation_from_public`]'s scoping so the
/// COPY-transported baseline equals the loopback-populated one.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn baseline_copy_out_select<C: GenericClient>(
    client: &mut C,
    table: &str,
    corpus: &str,
) -> Result<String, StorageError> {
    let columns = replicated_table_columns(client, table)?;
    let select_cols = columns
        .iter()
        .map(|col| format!("t.{}", sql_identifier(col)))
        .collect::<Vec<_>>()
        .join(", ");
    let predicate = corpus_scope_predicate(table, &sql_string_literal(corpus));
    // Deterministic row order (plan P3 NIT) — without it Postgres may return COPY rows in any physical
    // order, so the per-file binary digest would drift across equivalent builds. Order by the primary
    // key (the same key the §5.4 digest specs order by); fall back to every column if a table somehow
    // has no PK.
    let pk = primary_key_columns(client, table)?;
    let order_cols = if pk.is_empty() { &columns } else { &pk };
    let order_by = order_cols
        .iter()
        .map(|col| format!("t.{}", sql_identifier(col)))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!(
        "SELECT {select_cols} FROM public.{} t {predicate} ORDER BY {order_by}",
        sql_identifier(table)
    ))
}

/// The primary-key columns of a `public` table, in key order (used to make the baseline COPY-out
/// deterministic). Empty if the table has no primary key.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn primary_key_columns<C: GenericClient>(
    client: &mut C,
    table: &str,
) -> Result<Vec<String>, StorageError> {
    let regclass = format!("public.{table}");
    let rows = client
        .query(
            "SELECT a.attname \
             FROM pg_index i \
             JOIN unnest(i.indkey::int2[]) WITH ORDINALITY AS k(attnum, ord) ON true \
             JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum \
             WHERE i.indrelid = $1::text::regclass AND i.indisprimary \
             ORDER BY k.ord;",
            &[&regclass],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(rows
        .iter()
        .map(|row| row.get::<_, String>("attname"))
        .collect())
}

/// Copy a corpus's rows from the producer's `public` base tables into a generation schema — a
/// helper for tests and for cutting a baseline from the producer's own working set. (The client's
/// real apply loads from the package payload, not `public`.)
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn populate_generation_from_public<C: GenericClient>(
    client: &mut C,
    corpus: &str,
    generation: &str,
) -> Result<(), StorageError> {
    let schema = schema_for_generation(generation);
    let corpus_lit = sql_string_literal(corpus);
    for table in REPLICATED_TABLES {
        let columns = replicated_table_columns(client, table)?;
        let insert_cols = columns
            .iter()
            .map(|col| sql_identifier(col))
            .collect::<Vec<_>>()
            .join(", ");
        let select_cols = columns
            .iter()
            .map(|col| format!("t.{}", sql_identifier(col)))
            .collect::<Vec<_>>()
            .join(", ");
        let predicate = corpus_scope_predicate(table, &corpus_lit);
        let sql = format!(
            "INSERT INTO {dst}.{table} ({insert_cols}) \
             SELECT {select_cols} FROM public.{table} t {predicate};",
            dst = sql_identifier(&schema),
            table = sql_identifier(table),
        );
        client
            .batch_execute(&sql)
            .map_err(StorageError::PostgresClient)?;
    }
    Ok(())
}

/// Cut a new generation for `corpus` from the producer's `public` working set in one atomic call:
/// register + clone the schema ([`create_generation_schema`]) and copy the corpus's rows into it
/// ([`populate_generation_from_public`]), in a single transaction so a half-built generation is never
/// left behind. Returns the generation label (e.g. `core_g0001`); the caller validates and
/// [`activate_generation`]s it. A convenience for callers that hold a [`ManagedPostgres`] rather than a
/// client (the P3 baseline-from-public path, and tests).
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn create_generation_from_public(
    postgres: &ManagedPostgres,
    corpus: &str,
    counter: u32,
    source_baseline_id: Option<&str>,
) -> Result<String, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    let generation = create_generation_schema(&mut tx, corpus, counter, source_baseline_id)?;
    populate_generation_from_public(&mut tx, corpus, &generation)?;
    tx.commit().map_err(StorageError::PostgresClient)?;
    Ok(generation)
}

/// The `WHERE` clause that scopes a replicated table's rows to one corpus, by the owning document's
/// corpus (or the explicit `corpus` column). Mirrors the P0 attribution + the §5.4 digest scoping.
fn corpus_scope_predicate(table: &str, corpus_lit: &str) -> String {
    let owner_exists = |from_join: &str| {
        format!("WHERE EXISTS (SELECT 1 FROM {from_join} AND d.corpus = {corpus_lit})")
    };
    match table {
        "documents" | "official_api_responses" | "legislation_citation_resolutions" => {
            format!("WHERE t.corpus = {corpus_lit}")
        }
        // LEGI-only: present iff the corpus is the one LEGI belongs to.
        "legi_metadata_roots" => format!("WHERE {corpus_lit} = 'core'"),
        // Directly document-keyed.
        "chunks" => owner_exists("public.documents d WHERE d.document_id = t.document_id"),
        "graph_edges" => {
            owner_exists("public.documents d WHERE d.document_id = t.from_document_id")
        }
        "zone_units" => owner_exists("public.documents d WHERE d.document_id = t.document_id"),
        "decision_zones" => owner_exists("public.documents d WHERE d.document_id = t.document_id"),
        "decision_legislation_citations" => {
            owner_exists("public.documents d WHERE d.document_id = t.decision_document_id")
        }
        // Two-hop: embeddings reference their parent (chunk / zone_unit), which references documents.
        "chunk_embeddings" => owner_exists(
            "public.chunks c JOIN public.documents d ON d.document_id = c.document_id \
             WHERE c.chunk_id = t.chunk_id",
        ),
        "zone_unit_embeddings" => owner_exists(
            "public.zone_units z JOIN public.documents d ON d.document_id = z.document_id \
             WHERE z.zone_unit_id = t.zone_unit_id",
        ),
        other => panic!("corpus_scope_predicate: unclassified replicated table `{other}`"),
    }
}

/// Resolve the active physical generation schema for a corpus from `corpus_state` (the §7.2
/// activation authority) — used by the hot indexed retrieval path to target the qualified physical
/// tables. `None` when the corpus is not installed.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn active_generation_schema<C: GenericClient>(
    client: &mut C,
    corpus: &str,
) -> Result<Option<String>, StorageError> {
    let row = client
        .query_opt(
            "SELECT active_generation FROM jurisearch_control.corpus_state WHERE corpus = $1;",
            &[&corpus],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(row.map(|row| schema_for_generation(&row.get::<_, String>("active_generation"))))
}

/// Every (corpus, active physical schema) the client currently serves, in corpus order.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn active_generation_schemas<C: GenericClient>(
    client: &mut C,
) -> Result<Vec<(String, String)>, StorageError> {
    let rows = client
        .query(
            "SELECT corpus, active_generation FROM jurisearch_control.corpus_state ORDER BY corpus;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let corpus: String = row.get("corpus");
            let generation: String = row.get("active_generation");
            (corpus, schema_for_generation(&generation))
        })
        .collect())
}

/// Rebuild every `jurisearch_server.<relation>` view as `UNION ALL` over the active per-corpus
/// generations (design §4.3). With no active corpus a view selects the public shape with `WHERE
/// false` (correct columns, zero rows — never stale public data). Idempotent; called after every
/// activation/retire.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn rebuild_server_views<C: GenericClient>(client: &mut C) -> Result<(), StorageError> {
    let active = active_generation_schemas(client)?;
    let mut ddl = String::new();
    for table in REPLICATED_TABLES {
        let body = if active.is_empty() {
            format!("SELECT * FROM public.{} WHERE false", sql_identifier(table))
        } else {
            active
                .iter()
                .map(|(_corpus, schema)| {
                    format!(
                        "SELECT * FROM {}.{}",
                        sql_identifier(schema),
                        sql_identifier(table)
                    )
                })
                .collect::<Vec<_>>()
                .join(" UNION ALL ")
        };
        ddl.push_str(&format!(
            "CREATE OR REPLACE VIEW {}.{} AS {body};\n",
            sql_identifier(SERVER_VIEW_SCHEMA),
            sql_identifier(table),
        ));
    }
    client
        .batch_execute(&ddl)
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// The compatibility stamps recorded on activation (mirrors the contract crate's stamp set).
#[derive(Debug, Clone)]
pub struct ActivationStamps<'a> {
    pub sequence: i64,
    pub baseline_id: &'a str,
    pub schema_version: i32,
    pub embedding_fingerprint: &'a str,
    pub builder_versions: &'a serde_json::Value,
    pub last_package_id: Option<&'a str>,
    pub last_package_digest: Option<&'a str>,
}

/// Activate a `building` generation for a corpus and repoint that corpus's views in ONE short
/// transaction (design §7.4): take the package-apply advisory lock with a low `lock_timeout`
/// (fail-clean), validate the target registry row is `building` and the current cursor matches
/// `expected_previous_sequence` (the §7.3 cursor guard; `None` for the first baseline), retire the
/// old active generation, mark this one `active`, write the `corpus_state` cursor (this function is
/// the **sole writer** of `corpus_state`), rebuild the stable views, and commit as a unit. A failure
/// after the cursor advance (e.g. a view-rebuild error) rolls the whole switch back — no half-state.
///
/// # Errors
/// [`StorageError::PostgresClient`] / [`StorageError::AdvisoryLockBusy`] on lock contention, or
/// [`StorageError::Generations`] if the target row is not `building` or the cursor is unexpected.
pub fn activate_generation(
    postgres: &ManagedPostgres,
    corpus: &str,
    generation: &str,
    stamps: &ActivationStamps<'_>,
    expected_previous_sequence: Option<i64>,
) -> Result<(), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    tx.batch_execute("SET LOCAL lock_timeout = '5s';")
        .map_err(StorageError::PostgresClient)?;
    // Apply advisory lock for this transaction; fail clean rather than stall behind a long reader.
    let got: bool = tx
        .query_one(
            "SELECT pg_try_advisory_xact_lock($1);",
            &[&APPLY_ADVISORY_LOCK_KEY],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    if !got {
        return Err(StorageError::Generations {
            message: "another apply/switch holds the advisory lock".to_owned(),
        });
    }

    // The target generation must be `building` (cannot activate an unbuilt / already-retired row).
    let state: Option<String> = tx
        .query_opt(
            "SELECT state FROM jurisearch_control.generation_registry \
             WHERE corpus = $1 AND generation = $2 FOR UPDATE;",
            &[&corpus, &generation],
        )
        .map_err(StorageError::PostgresClient)?
        .map(|row| row.get("state"));
    match state.as_deref() {
        Some("building") => {}
        Some(other) => {
            return Err(StorageError::Generations {
                message: format!(
                    "cannot activate generation `{generation}` in state `{other}` (expected building)"
                ),
            });
        }
        None => {
            return Err(StorageError::Generations {
                message: format!(
                    "generation `{generation}` is not registered for corpus `{corpus}`"
                ),
            });
        }
    }

    // The §7.3 cursor guard: ALWAYS read the current cursor (`FOR UPDATE`) and require it matches the
    // caller's expectation. `None` means "first baseline" → there must be NO existing `corpus_state`
    // row; `Some(n)` means the cursor must currently be exactly `n`. Crucially, `None` against an
    // already-installed corpus is rejected (not silently accepted) so a stale/miswired switch cannot
    // clobber a live cursor by passing `None`.
    let current_sequence: Option<i64> = tx
        .query_opt(
            "SELECT sequence FROM jurisearch_control.corpus_state WHERE corpus = $1 FOR UPDATE;",
            &[&corpus],
        )
        .map_err(StorageError::PostgresClient)?
        .map(|row| row.get("sequence"));
    match (expected_previous_sequence, current_sequence) {
        (None, None) => {}
        (Some(expected), Some(current)) if current == expected => {}
        (expected, found) => {
            return Err(StorageError::Generations {
                message: format!(
                    "cursor mismatch for `{corpus}`: expected previous sequence {expected:?}, \
                     found {found:?}"
                ),
            });
        }
    }

    tx.execute(
        "UPDATE jurisearch_control.generation_registry \
         SET state = 'retired', retired_at = now() \
         WHERE corpus = $1 AND state = 'active' AND generation <> $2;",
        &[&corpus, &generation],
    )
    .map_err(StorageError::PostgresClient)?;
    tx.execute(
        "UPDATE jurisearch_control.generation_registry \
         SET state = 'active', activated_at = now(), source_package_id = $3, validation_digest = $4 \
         WHERE corpus = $1 AND generation = $2;",
        &[
            &corpus,
            &generation,
            &stamps.last_package_id,
            &stamps.last_package_digest,
        ],
    )
    .map_err(StorageError::PostgresClient)?;

    let builder_versions = serde_json::to_string(stamps.builder_versions)?;
    tx.execute(
        "INSERT INTO jurisearch_control.corpus_state \
             (corpus, active_generation, sequence, baseline_id, schema_version, \
              embedding_fingerprint, builder_versions, last_package_id, last_package_digest, applied_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7::text::jsonb,$8,$9, now()) \
         ON CONFLICT (corpus) DO UPDATE SET \
             active_generation = EXCLUDED.active_generation, \
             sequence = EXCLUDED.sequence, \
             baseline_id = EXCLUDED.baseline_id, \
             schema_version = EXCLUDED.schema_version, \
             embedding_fingerprint = EXCLUDED.embedding_fingerprint, \
             builder_versions = EXCLUDED.builder_versions, \
             last_package_id = EXCLUDED.last_package_id, \
             last_package_digest = EXCLUDED.last_package_digest, \
             applied_at = now();",
        &[
            &corpus,
            &generation,
            &stamps.sequence,
            &stamps.baseline_id,
            &stamps.schema_version,
            &stamps.embedding_fingerprint,
            &builder_versions,
            &stamps.last_package_id,
            &stamps.last_package_digest,
        ],
    )
    .map_err(StorageError::PostgresClient)?;

    rebuild_server_views(&mut tx)?;
    tx.commit().map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// Async cleanup of a `retired` generation: drop its physical schema and the registry row. This is
/// the operated cleanup path (a bounded drop after the switch is validated, §7.4) — distinct from the
/// disaster-recovery `DROP SCHEMA … CASCADE` that the operated *switch* never performs.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn drop_retired_generation(
    postgres: &ManagedPostgres,
    corpus: &str,
    generation: &str,
) -> Result<(), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    tx.batch_execute("SET LOCAL lock_timeout = '5s';")
        .map_err(StorageError::PostgresClient)?;
    // Look up the row and verify it is `retired` (FOR UPDATE so we cannot race activation) BEFORE
    // dropping anything — never drop an active/building/misspelled generation, and use the stored
    // physical_schema rather than re-deriving it.
    let physical_schema: String = tx
        .query_opt(
            "SELECT physical_schema FROM jurisearch_control.generation_registry \
             WHERE corpus = $1 AND generation = $2 AND state = 'retired' FOR UPDATE;",
            &[&corpus, &generation],
        )
        .map_err(StorageError::PostgresClient)?
        .map(|row| row.get("physical_schema"))
        .ok_or_else(|| StorageError::Generations {
            message: format!(
                "refusing to drop generation `{generation}` for `{corpus}`: no retired registry row"
            ),
        })?;
    tx.batch_execute(&format!(
        "DROP SCHEMA {} CASCADE;",
        sql_identifier(&physical_schema)
    ))
    .map_err(StorageError::PostgresClient)?;
    tx.execute(
        "DELETE FROM jurisearch_control.generation_registry \
         WHERE corpus = $1 AND generation = $2 AND state = 'retired';",
        &[&corpus, &generation],
    )
    .map_err(StorageError::PostgresClient)?;
    tx.commit().map_err(StorageError::PostgresClient)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replicated_and_non_generation_tables_are_disjoint() {
        for table in REPLICATED_TABLES {
            assert!(
                !NON_GENERATION_TABLES.contains(table),
                "`{table}` cannot be both replicated and non-generation"
            );
            assert!(is_replicated_table(table));
        }
        for table in NON_GENERATION_TABLES {
            assert!(!is_replicated_table(table));
        }
    }

    #[test]
    fn generation_naming_is_zero_padded_and_schema_prefixed() {
        assert_eq!(generation_name("core", 1), "core_g0001");
        assert_eq!(
            generation_schema("core", 12),
            "jurisearch_server_core_g0012"
        );
        assert_eq!(
            schema_for_generation("core_g0001"),
            "jurisearch_server_core_g0001"
        );
    }
}
