//! P4 incremental apply primitives: apply a diff's rows onto a generation schema, and the incremental
//! cursor writer. The rows travel as JSON objects (one per JSONL line); a row is inserted via
//! `jsonb_populate_record(null::<schema>.<table>, row)` projected to the non-generated column list, so
//! typed columns (incl. `vector`, rendered by `to_jsonb` as a string) round-trip exactly. Replace-sets
//! delete the scope's current rows (relying on the generation's recreated `ON DELETE CASCADE` FKs) then
//! re-insert. The whole apply runs in the CALLER's transaction against the ACTIVE generation (§7.3).

use std::collections::BTreeMap;

use jurisearch_package::event::ReplaceSetGroup;
use postgres::GenericClient;
use serde_json::Value;

use crate::generations::replicated_table_columns;
use crate::runtime::{StorageError, sql_identifier, sql_string_literal};

/// `jsonb_build_object('c1', t."c1", …)` over the non-generated columns — the exact row JSON moved by an
/// incremental (vectors render as strings via `to_jsonb`, which `jsonb_populate_record` round-trips).
#[must_use]
pub fn row_object_select(columns: &[String]) -> String {
    let pairs = columns
        .iter()
        .map(|c| format!("'{c}', t.{}", sql_identifier(c)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("jsonb_build_object({pairs})")
}

/// The per-table SELECT reading a replace-set table's rows for `document_id` in `schema`, ordered by PK.
/// `chunk_embeddings`/`zone_unit_embeddings` are joined to their parent so the scope is the document.
fn replace_set_table_select(
    schema: &str,
    table: &str,
    columns: &[String],
    document_id: &str,
) -> String {
    let s = sql_identifier(schema);
    let obj = row_object_select(columns);
    let key = sql_string_literal(document_id);
    match table {
        "chunks" => {
            format!("SELECT {obj} FROM {s}.chunks t WHERE document_id = {key} ORDER BY chunk_id;")
        }
        "chunk_embeddings" => format!(
            "SELECT {obj} FROM {s}.chunk_embeddings t \
             WHERE chunk_id IN (SELECT chunk_id FROM {s}.chunks WHERE document_id = {key}) \
             ORDER BY chunk_id;"
        ),
        "zone_units" => format!(
            "SELECT {obj} FROM {s}.zone_units t WHERE document_id = {key} ORDER BY zone_unit_id;"
        ),
        "zone_unit_embeddings" => format!(
            "SELECT {obj} FROM {s}.zone_unit_embeddings t \
             WHERE zone_unit_id IN (SELECT zone_unit_id FROM {s}.zone_units WHERE document_id = {key}) \
             ORDER BY zone_unit_id;"
        ),
        "decision_zones" => format!(
            "SELECT {obj} FROM {s}.decision_zones t WHERE document_id = {key} ORDER BY document_id;"
        ),
        _ => format!(
            "SELECT {obj} FROM {s}.{} t WHERE FALSE;",
            sql_identifier(table)
        ),
    }
}

/// The payload tables (parent before child) of a replace-set group.
#[must_use]
pub fn group_payload_tables(group: ReplaceSetGroup) -> &'static [&'static str] {
    match group {
        ReplaceSetGroup::ZoneUnits => &["zone_units", "zone_unit_embeddings"],
        ReplaceSetGroup::ChunksWithEmbeddings => &["chunks", "chunk_embeddings"],
        ReplaceSetGroup::ChunkEmbeddings => &["chunk_embeddings"],
        ReplaceSetGroup::DecisionZones => &["decision_zones"],
    }
}

/// Read a replace-set scope's CURRENT rows from `schema` as the per-table `rows` map (plan P4). The
/// builder reads from `public`; the applier reads from the active generation post-apply to recompute
/// `set_digest` — the SINGLE read path, so both digests are computed over identically-shaped rows.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn replace_set_rows<C: GenericClient>(
    client: &mut C,
    schema: &str,
    group: ReplaceSetGroup,
    document_id: &str,
) -> Result<BTreeMap<String, Vec<Value>>, StorageError> {
    let mut rows = BTreeMap::new();
    for table in group_payload_tables(group) {
        let columns = replicated_table_columns(client, table)?;
        let select = replace_set_table_select(schema, table, &columns, document_id);
        let result = client
            .query(&select, &[])
            .map_err(StorageError::PostgresClient)?;
        rows.insert(
            (*table).to_owned(),
            result.iter().map(|r| r.get::<_, Value>(0)).collect(),
        );
    }
    Ok(rows)
}

/// Insert/replace one set of rows into `<schema>.<table>` with `ON CONFLICT (<pk>) DO UPDATE` over every
/// non-PK column (INV-1: an upsert replicates in-place updates like a closing `valid_to`, not just
/// inserts). `columns` is the non-generated column list; `pk_columns` the primary key.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn apply_upserts<C: GenericClient>(
    client: &mut C,
    schema: &str,
    table: &str,
    columns: &[String],
    pk_columns: &[String],
    rows: &[Value],
) -> Result<u64, StorageError> {
    if rows.is_empty() {
        return Ok(0);
    }
    let schema_ident = sql_identifier(schema);
    let table_ident = sql_identifier(table);
    let col_list = columns
        .iter()
        .map(|c| sql_identifier(c))
        .collect::<Vec<_>>()
        .join(", ");
    let select_list = columns
        .iter()
        .map(|c| format!("rec.{}", sql_identifier(c)))
        .collect::<Vec<_>>()
        .join(", ");
    let pk_set: std::collections::BTreeSet<&str> = pk_columns.iter().map(String::as_str).collect();
    let updates = columns
        .iter()
        .filter(|c| !pk_set.contains(c.as_str()))
        .map(|c| format!("{col} = EXCLUDED.{col}", col = sql_identifier(c)))
        .collect::<Vec<_>>()
        .join(", ");
    let conflict = pk_columns
        .iter()
        .map(|c| sql_identifier(c))
        .collect::<Vec<_>>()
        .join(", ");
    let on_conflict = if updates.is_empty() {
        format!("ON CONFLICT ({conflict}) DO NOTHING")
    } else {
        format!("ON CONFLICT ({conflict}) DO UPDATE SET {updates}")
    };
    let sql = format!(
        "INSERT INTO {schema_ident}.{table_ident} ({col_list}) \
         SELECT {select_list} \
         FROM jsonb_populate_record(null::{schema_ident}.{table_ident}, $1::text::jsonb) rec \
         {on_conflict};"
    );
    let mut applied = 0u64;
    for row in rows {
        let json = serde_json::to_string(row)?;
        applied += client
            .execute(&sql, &[&json])
            .map_err(StorageError::PostgresClient)?;
    }
    Ok(applied)
}

/// Delete rows from `<schema>.<table>` whose primary key matches each JSON key object.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn apply_deletes<C: GenericClient>(
    client: &mut C,
    schema: &str,
    table: &str,
    pk_columns: &[String],
    keys: &[Value],
) -> Result<u64, StorageError> {
    if keys.is_empty() {
        return Ok(0);
    }
    let schema_ident = sql_identifier(schema);
    let table_ident = sql_identifier(table);
    let predicate = pk_columns
        .iter()
        .map(|c| format!("t.{col} = k.{col}", col = sql_identifier(c)))
        .collect::<Vec<_>>()
        .join(" AND ");
    let sql = format!(
        "DELETE FROM {schema_ident}.{table_ident} t \
         USING jsonb_populate_record(null::{schema_ident}.{table_ident}, $1::text::jsonb) k \
         WHERE {predicate};"
    );
    let mut deleted = 0u64;
    for key in keys {
        let json = serde_json::to_string(key)?;
        deleted += client
            .execute(&sql, &[&json])
            .map_err(StorageError::PostgresClient)?;
    }
    Ok(deleted)
}

/// Plain INSERT of `rows` into `<schema>.<table>` (used after a replace-set delete cleared the scope).
fn insert_rows<C: GenericClient>(
    client: &mut C,
    schema: &str,
    table: &str,
    columns: &[String],
    rows: &[Value],
) -> Result<(), StorageError> {
    if rows.is_empty() {
        return Ok(());
    }
    let schema_ident = sql_identifier(schema);
    let table_ident = sql_identifier(table);
    let col_list = columns
        .iter()
        .map(|c| sql_identifier(c))
        .collect::<Vec<_>>()
        .join(", ");
    let select_list = columns
        .iter()
        .map(|c| format!("rec.{}", sql_identifier(c)))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {schema_ident}.{table_ident} ({col_list}) \
         SELECT {select_list} \
         FROM jsonb_populate_record(null::{schema_ident}.{table_ident}, $1::text::jsonb) rec;"
    );
    for row in rows {
        let json = serde_json::to_string(row)?;
        client
            .execute(&sql, &[&json])
            .map_err(StorageError::PostgresClient)?;
    }
    Ok(())
}

/// The (parent table, child embedding table) and the delete predicate column for a replace-set group.
/// `ChunkEmbeddings` deletes only the embeddings for the document's chunks (the chunk set is unchanged).
fn group_tables(group: ReplaceSetGroup) -> &'static [&'static str] {
    match group {
        ReplaceSetGroup::ZoneUnits => &["zone_units", "zone_unit_embeddings"],
        ReplaceSetGroup::ChunksWithEmbeddings => &["chunks", "chunk_embeddings"],
        ReplaceSetGroup::ChunkEmbeddings => &["chunk_embeddings"],
        ReplaceSetGroup::DecisionZones => &["decision_zones"],
    }
}

/// Apply one `replace_set` scope to `<schema>` (plan P4 D5): delete the scope's current rows (a parent
/// delete relies on the generation's `ON DELETE CASCADE` FK to clear children — that is what guarantees
/// no stale BM25-visible chunk survives), then insert the package's rows in dependency order
/// (parents before children). `columns_for` returns the non-generated column list for a table.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error, or [`StorageError::Generations`] on a malformed set.
pub fn apply_replace_set<C: GenericClient>(
    client: &mut C,
    schema: &str,
    group: ReplaceSetGroup,
    document_id: &str,
    rows: &BTreeMap<String, Vec<Value>>,
    mut columns_for: impl FnMut(&str) -> Result<Vec<String>, StorageError>,
) -> Result<(), StorageError> {
    let schema_ident = sql_identifier(schema);
    let doc_lit = sql_string_literal(document_id);
    // Delete the scope's current rows. For chunk/zone/decision groups the parent is keyed by
    // `document_id`; `ChunkEmbeddings` deletes only the embeddings for the document's chunks.
    let delete_sql = match group {
        ReplaceSetGroup::ZoneUnits => {
            format!("DELETE FROM {schema_ident}.zone_units WHERE document_id = {doc_lit};")
        }
        ReplaceSetGroup::ChunksWithEmbeddings => {
            format!("DELETE FROM {schema_ident}.chunks WHERE document_id = {doc_lit};")
        }
        ReplaceSetGroup::ChunkEmbeddings => format!(
            "DELETE FROM {schema_ident}.chunk_embeddings WHERE chunk_id IN \
             (SELECT chunk_id FROM {schema_ident}.chunks WHERE document_id = {doc_lit});"
        ),
        ReplaceSetGroup::DecisionZones => {
            format!("DELETE FROM {schema_ident}.decision_zones WHERE document_id = {doc_lit};")
        }
    };
    client
        .batch_execute(&delete_sql)
        .map_err(StorageError::PostgresClient)?;

    // Insert the package rows, parents before children.
    for table in group_tables(group) {
        if let Some(table_rows) = rows.get(*table) {
            let columns = columns_for(table)?;
            insert_rows(client, schema, table, &columns, table_rows)?;
        }
    }
    Ok(())
}

/// Advance the `corpus_state` cursor after an incremental apply (plan P4 D4 — the INCREMENTAL cursor
/// writer, distinct from `generations::activate_generation`, the SWITCH writer). Updates the sequence +
/// last-package identity in place (the active generation and baseline are unchanged). Runs in the
/// caller's apply transaction.
///
/// # Errors
/// [`StorageError::Generations`] if the corpus has no cursor row; [`StorageError::PostgresClient`] on a
/// DB error.
pub fn advance_corpus_cursor<C: GenericClient>(
    client: &mut C,
    corpus: &str,
    new_sequence: i64,
    last_package_id: &str,
    last_package_digest: &str,
) -> Result<(), StorageError> {
    let updated = client
        .execute(
            "UPDATE jurisearch_control.corpus_state \
             SET sequence = $2, last_package_id = $3, last_package_digest = $4, applied_at = now() \
             WHERE corpus = $1;",
            &[
                &corpus,
                &new_sequence,
                &last_package_id,
                &last_package_digest,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    if updated != 1 {
        return Err(StorageError::Generations {
            message: format!("no corpus_state row to advance for corpus `{corpus}`"),
        });
    }
    Ok(())
}

/// Whether `<schema>` has the cascade FKs the replace-set stale-row guarantee depends on (plan P4 D5):
/// `chunk_embeddings → chunks` and `zone_unit_embeddings → zone_units`, both `ON DELETE CASCADE`.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn has_cascade_fks<C: GenericClient>(
    client: &mut C,
    schema: &str,
) -> Result<bool, StorageError> {
    let schema_lit = sql_string_literal(schema);
    let count: i64 = client
        .query_one(
            &format!(
                "SELECT count(*)::bigint FROM pg_constraint c \
                 JOIN pg_class t ON t.oid = c.conrelid \
                 JOIN pg_namespace n ON n.oid = t.relnamespace \
                 WHERE n.nspname = {schema_lit} AND c.contype = 'f' AND c.confdeltype = 'c' \
                   AND t.relname IN ('chunk_embeddings', 'zone_unit_embeddings');"
            ),
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    Ok(count >= 2)
}
