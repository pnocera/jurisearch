//! The change-capture outbox (`package_change_log`, migration v19; design §5.1, plan P1).
//!
//! Every replicated-table writer emits **one ledger row per changed scope, in the mutation's own
//! transaction**, so incremental package diffs are computable without a uniform `updated_at` (C7)
//! and without snapshot diffing or logical decoding as the primary path. The ledger records the
//! *scope touched* (corpus, table, op, scope) — not necessarily full row bodies; the package builder
//! rematerialises payloads from the authoritative tables at build time (§5.1).
//!
//! Two coordinates, kept distinct (§5.1): `change_seq` here is the **global** build/audit order; the
//! per-corpus **package sequence** is assigned later by the builder/catalog. The read API
//! ([`scopes_changed_for_corpus`]) reasons only in `change_seq` space — never package-sequence space
//! — which is exactly what prevents a cross-corpus false `sequence_gap`.

use crate::runtime::{ManagedPostgres, StorageError, sql_string_literal};
use jurisearch_package::ChangeSeq;
use jurisearch_package::event::EventKind;
use postgres::GenericClient;

/// Scope-kind tokens used in `package_change_log.scope_kind`. Free text in the schema, fixed here so
/// emitters and the read API agree (the contract crate's `ScopeKind` covers only the manifest's
/// `document`/`logical_article`; the outbox needs a few more for non-document-owned tables).
pub mod scope_kind {
    /// A specific document/decision, keyed by `document_id`.
    pub const DOCUMENT: &str = "document";
    /// A LEGI metadata root (TEXTE_VERSION / section), keyed by its root id.
    pub const LEGI_METADATA_ROOT: &str = "legi_metadata_root";
    /// A deduped legislation-citation resolution, keyed by `citation_key`.
    pub const CITATION_RESOLUTION: &str = "citation_resolution";
    /// One archived official-API exchange, keyed by the producer `response_id`.
    pub const OFFICIAL_API_RESPONSE: &str = "official_api_response";
}

/// Run-level constants threaded into each replicated-table writer (design §5.1). One per producer
/// mutation run (a LEGI/juri archive ingest, an embedding job, a zone/citation enrichment, a
/// hierarchy backfill); each command mints its own at start.
#[derive(Debug, Clone, Copy)]
pub struct OutboxContext<'a> {
    /// The run that produced this mutation (audit; `package_change_log.ingest_run_id`).
    pub ingest_run_id: &'a str,
    /// The storage `schema_version` the mutation was written under.
    pub schema_version: i32,
}

impl<'a> OutboxContext<'a> {
    #[must_use]
    pub fn new(ingest_run_id: &'a str, schema_version: i32) -> Self {
        Self {
            ingest_run_id,
            schema_version,
        }
    }
}

/// One semantic change to record (design §5.1). Construct the common "scope touched" case with
/// [`OutboxEvent::scope`]; the hash/payload/stamp fields are optional and usually left to the
/// build-time rematerialisation.
#[derive(Debug, Clone)]
pub struct OutboxEvent<'a> {
    pub corpus: &'a str,
    pub table_name: &'a str,
    pub op: EventKind,
    pub scope_kind: &'a str,
    pub scope_key: &'a str,
    pub row_pk: Option<&'a serde_json::Value>,
    pub row_hash: Option<&'a str>,
    pub before_hash: Option<&'a str>,
    pub after_hash: Option<&'a str>,
    pub payload: Option<&'a serde_json::Value>,
    pub builder_versions: Option<&'a serde_json::Value>,
    pub embedding_fingerprint: Option<&'a str>,
}

impl<'a> OutboxEvent<'a> {
    /// The common case: record only that a scope changed (corpus/table/op/scope), leaving the
    /// optional hash/payload/stamp fields NULL for build-time rematerialisation (§5.1).
    #[must_use]
    pub fn scope(
        corpus: &'a str,
        table_name: &'a str,
        op: EventKind,
        scope_kind: &'a str,
        scope_key: &'a str,
    ) -> Self {
        Self {
            corpus,
            table_name,
            op,
            scope_kind,
            scope_key,
            row_pk: None,
            row_hash: None,
            before_hash: None,
            after_hash: None,
            payload: None,
            builder_versions: None,
            embedding_fingerprint: None,
        }
    }
}

/// Emit one ledger row in the caller's transaction (design §5.1). Returns the assigned global
/// `change_seq`. Because this runs on the same `client` as the mutation, a rollback of that
/// transaction discards the ledger row too — no orphan outbox rows (INV: emit-in-same-txn).
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error, or [`StorageError::Outbox`] if the returned
/// `change_seq` is somehow negative.
pub fn emit_change<C: GenericClient>(
    client: &mut C,
    ctx: &OutboxContext<'_>,
    event: &OutboxEvent<'_>,
) -> Result<ChangeSeq, StorageError> {
    let op = event.op.as_str();
    // jsonb columns are passed as text + cast (mirrors the other parameterized writers).
    let row_pk = serialize_json_param(event.row_pk)?;
    let payload = serialize_json_param(event.payload)?;
    let builder_versions = serialize_json_param(event.builder_versions)?;
    let inserted = client
        .query_one(
            "INSERT INTO package_change_log (\
                 corpus, ingest_run_id, table_name, op, scope_kind, scope_key, \
                 row_pk, row_hash, before_hash, after_hash, payload, builder_versions, \
                 embedding_fingerprint, schema_version) \
             VALUES ($1,$2,$3,$4,$5,$6, \
                 COALESCE($7::text::jsonb, '{}'::jsonb), $8, $9, $10, $11::text::jsonb, \
                 COALESCE($12::text::jsonb, '{}'::jsonb), $13, $14) \
             RETURNING change_seq;",
            &[
                &event.corpus,
                &ctx.ingest_run_id,
                &event.table_name,
                &op,
                &event.scope_kind,
                &event.scope_key,
                &row_pk,
                &event.row_hash,
                &event.before_hash,
                &event.after_hash,
                &payload,
                &builder_versions,
                &event.embedding_fingerprint,
                &ctx.schema_version,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    let seq: i64 = inserted.get("change_seq");
    ChangeSeq::from_db(seq).map_err(|error| StorageError::Outbox {
        message: error.to_string(),
    })
}

fn serialize_json_param(value: Option<&serde_json::Value>) -> Result<Option<String>, StorageError> {
    value
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| StorageError::Outbox {
            message: format!("serialize outbox json: {error}"),
        })
}

/// One changed scope read back from the ledger (the authoritative "what changed").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedScope {
    pub change_seq: ChangeSeq,
    pub table_name: String,
    pub op: EventKind,
    pub scope_kind: String,
    pub scope_key: String,
}

/// The §5.1 read API: the scopes changed for `corpus` in the **global `change_seq`** half-open range
/// `(after, through]`, in `change_seq` order. This is the authoritative diff source for the builder;
/// it never reasons in package-sequence space (which is what prevents a cross-corpus `sequence_gap`).
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error, or [`StorageError::Outbox`] on a malformed `op`.
pub fn scopes_changed_for_corpus(
    postgres: &ManagedPostgres,
    corpus: &str,
    after: ChangeSeq,
    through: ChangeSeq,
) -> Result<Vec<ChangedScope>, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let after = i64::try_from(after.get()).unwrap_or(i64::MAX);
    let through = i64::try_from(through.get()).unwrap_or(i64::MAX);
    let rows = client
        .query(
            "SELECT change_seq, table_name, op, scope_kind, scope_key \
             FROM package_change_log \
             WHERE corpus = $1 AND change_seq > $2 AND change_seq <= $3 \
             ORDER BY change_seq;",
            &[&corpus, &after, &through],
        )
        .map_err(StorageError::PostgresClient)?;
    rows.into_iter()
        .map(|row| {
            let op_text: String = row.get("op");
            let op = parse_op(&op_text)?;
            let seq: i64 = row.get("change_seq");
            Ok(ChangedScope {
                change_seq: ChangeSeq::from_db(seq).map_err(|error| StorageError::Outbox {
                    message: error.to_string(),
                })?,
                table_name: row.get("table_name"),
                op,
                scope_kind: row.get("scope_kind"),
                scope_key: row.get("scope_key"),
            })
        })
        .collect()
}

/// The current global `change_seq` high-watermark (max), or [`ChangeSeq::ZERO`] when the ledger is
/// empty. Used to seed the first package catalog window (P3) and to freeze a build watermark (P4).
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn current_change_seq(postgres: &ManagedPostgres) -> Result<ChangeSeq, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let row = client
        .query_one(
            "SELECT COALESCE(max(change_seq), 0) AS hi FROM package_change_log;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let hi: i64 = row.get("hi");
    ChangeSeq::from_db(hi).map_err(|error| StorageError::Outbox {
        message: error.to_string(),
    })
}

fn parse_op(text: &str) -> Result<EventKind, StorageError> {
    match text {
        "upsert" => Ok(EventKind::Upsert),
        "delete" => Ok(EventKind::Delete),
        "replace_set" => Ok(EventKind::ReplaceSet),
        other => Err(StorageError::Outbox {
            message: format!("unknown outbox op `{other}`"),
        }),
    }
}

/// QA backstop (§5.4, plan P1 "built here, not yet wired"): per-table row counts + a deterministic,
/// machine-independent content digest for one corpus, over the **authoritative tables** (independent
/// of the outbox hooks). Reused by the P3 baseline loopback proof and as a package postcondition.
///
/// The per-row signature is the **whole replicated row** as canonical jsonb minus only the
/// machine-local/time-volatile columns ([`VOLATILE_DIGEST_COLUMNS`]) — so a change to **any**
/// replicated column (body, citation, raw json, embedding vector, status, response id, …) changes
/// the digest. Rows are aggregated in `ORDER BY` of the primary key **inside the aggregate** so the
/// digest is order-deterministic regardless of scan order.
///
/// # Errors
/// [`StorageError::PostgresClient`] on a DB error.
pub fn corpus_table_digests(
    postgres: &ManagedPostgres,
    corpus: &str,
) -> Result<Vec<TableDigest>, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let corpus_lit = sql_string_literal(corpus);
    // `to_jsonb(<alias>) - 'created_at' - 'updated_at' - ...` drops volatile keys (a no-op when a key
    // is absent), leaving every replicated column in the signature.
    let strip_volatile: String = VOLATILE_DIGEST_COLUMNS
        .iter()
        .map(|col| format!(" - '{col}'"))
        .collect();
    let mut digests = Vec::with_capacity(REPLICATED_DIGEST_SPECS.len());
    for spec in REPLICATED_DIGEST_SPECS {
        let sql = format!(
            "WITH scoped AS (\
                 SELECT (to_jsonb({row_alias}){strip_volatile})::text AS sig, \
                        {order_by} AS sort_key \
                 FROM {from_clause} \
                 WHERE {corpus_predicate} = {corpus_lit}) \
             SELECT count(*)::bigint AS n, \
                    COALESCE(md5(string_agg(sig, '|' ORDER BY sort_key)), '') AS digest \
             FROM scoped;",
            row_alias = spec.row_alias,
            from_clause = spec.from_clause,
            corpus_predicate = spec.corpus_predicate,
            order_by = spec.order_by,
        );
        let row = client
            .query_one(&sql, &[])
            .map_err(StorageError::PostgresClient)?;
        let count: i64 = row.get("n");
        let digest: String = row.get("digest");
        digests.push(TableDigest {
            table_name: spec.table_name.to_owned(),
            row_count: u64::try_from(count).unwrap_or(0),
            digest: format!("md5:{digest}"),
        });
    }
    Ok(digests)
}

/// A per-table count + content digest for a corpus (the §5.4 QA backstop unit).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDigest {
    pub table_name: String,
    pub row_count: u64,
    pub digest: String,
}

/// Columns excluded from every digest: machine-local insert/update/fetch timestamps and TTL-derived
/// expiry, which differ across producer/client without being replicated *content*.
const VOLATILE_DIGEST_COLUMNS: &[&str] = &["created_at", "updated_at", "fetched_at", "expires_at"];

/// Per-replicated-table digest specification: how to scope it to a corpus and the row alias whose
/// **whole row** (minus volatile columns) forms the deterministic signature. The §4.2 replicated set.
struct DigestSpec {
    table_name: &'static str,
    from_clause: &'static str,
    corpus_predicate: &'static str,
    order_by: &'static str,
    /// The table alias passed to `to_jsonb(...)` — its full row is the content signature.
    row_alias: &'static str,
}

/// The design §4.2 replicated-table set and how each is captured (plan P1 risk: "a new replicated
/// table cannot ship without a hook"). This is an **enumerated** assertion, not a grep-discovered
/// inventory: every replicated table is either `Hooked` (an owned writer emits an outbox row),
/// `ClientBuilt` (indexes, never replicated as data), or `ControlOnly` (travels with the package
/// schema, rebuilt/stamped on the client — never an outbox scope).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureClass {
    /// An owned writer emits an outbox row (the §4.2 authoritative + enrichment set).
    Hooked,
    /// Control table: travels with the schema, not an outbox scope (§4.2 "Control (special)").
    ControlOnly,
}

#[cfg(test)]
const SECTION_4_2_TABLES: &[(&str, CaptureClass)] = &[
    // Authoritative corpus (replicate).
    ("documents", CaptureClass::Hooked),
    ("chunks", CaptureClass::Hooked),
    ("chunk_embeddings", CaptureClass::Hooked),
    ("graph_edges", CaptureClass::Hooked),
    ("legi_metadata_roots", CaptureClass::Hooked),
    ("zone_units", CaptureClass::Hooked),
    ("zone_unit_embeddings", CaptureClass::Hooked),
    ("decision_legislation_citations", CaptureClass::Hooked),
    ("legislation_citation_resolutions", CaptureClass::Hooked),
    // Enrichment/provenance (replicate — computed upfront).
    ("official_api_responses", CaptureClass::Hooked),
    ("decision_zones", CaptureClass::Hooked),
    // Control (special): travel with the schema, never an outbox scope.
    ("index_manifest", CaptureClass::ControlOnly),
    ("schema_migrations", CaptureClass::ControlOnly),
];

const REPLICATED_DIGEST_SPECS: &[DigestSpec] = &[
    DigestSpec {
        table_name: "documents",
        from_clause: "documents d",
        corpus_predicate: "d.corpus",
        order_by: "d.document_id",
        row_alias: "d",
    },
    DigestSpec {
        table_name: "chunks",
        from_clause: "chunks c JOIN documents d ON d.document_id = c.document_id",
        corpus_predicate: "d.corpus",
        order_by: "c.chunk_id",
        row_alias: "c",
    },
    DigestSpec {
        table_name: "chunk_embeddings",
        from_clause: "chunk_embeddings e JOIN chunks c ON c.chunk_id = e.chunk_id \
                      JOIN documents d ON d.document_id = c.document_id",
        corpus_predicate: "d.corpus",
        order_by: "e.chunk_id",
        row_alias: "e",
    },
    DigestSpec {
        table_name: "graph_edges",
        from_clause: "graph_edges g JOIN documents d ON d.document_id = g.from_document_id",
        corpus_predicate: "d.corpus",
        order_by: "g.edge_id",
        row_alias: "g",
    },
    DigestSpec {
        // LEGI metadata roots are always `core` (LEGI-only). The predicate `'core' = <corpus>`
        // selects all rows for the core corpus and none for any other.
        table_name: "legi_metadata_roots",
        from_clause: "legi_metadata_roots r",
        corpus_predicate: "'core'",
        order_by: "r.metadata_key",
        row_alias: "r",
    },
    DigestSpec {
        table_name: "zone_units",
        from_clause: "zone_units z JOIN documents d ON d.document_id = z.document_id",
        corpus_predicate: "d.corpus",
        order_by: "z.zone_unit_id",
        row_alias: "z",
    },
    DigestSpec {
        table_name: "zone_unit_embeddings",
        from_clause: "zone_unit_embeddings ze JOIN zone_units z ON z.zone_unit_id = ze.zone_unit_id \
                      JOIN documents d ON d.document_id = z.document_id",
        corpus_predicate: "d.corpus",
        order_by: "ze.zone_unit_id",
        row_alias: "ze",
    },
    DigestSpec {
        table_name: "decision_zones",
        from_clause: "decision_zones dz JOIN documents d ON d.document_id = dz.document_id",
        corpus_predicate: "d.corpus",
        order_by: "dz.document_id",
        row_alias: "dz",
    },
    DigestSpec {
        table_name: "decision_legislation_citations",
        from_clause: "decision_legislation_citations dlc \
                      JOIN documents d ON d.document_id = dlc.decision_document_id",
        corpus_predicate: "d.corpus",
        order_by: "dlc.citation_occurrence_id",
        row_alias: "dlc",
    },
    DigestSpec {
        table_name: "legislation_citation_resolutions",
        from_clause: "legislation_citation_resolutions res",
        corpus_predicate: "res.corpus",
        order_by: "res.citation_key",
        row_alias: "res",
    },
    DigestSpec {
        table_name: "official_api_responses",
        from_clause: "official_api_responses o",
        corpus_predicate: "o.corpus",
        order_by: "o.response_id",
        row_alias: "o",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Plan P1 coverage backstop (the §4.2 risk): the replicated set is an **enumerated** list, and
    /// every data table that is `Hooked` has a QA digest spec (so a new replicated table cannot ship
    /// without both a hook and a postcondition digest). Control tables are intentionally hookless.
    #[test]
    fn section_4_2_replicated_set_is_fully_classified() {
        // Every Hooked data table has exactly one digest spec, and vice versa.
        let hooked: std::collections::BTreeSet<&str> = SECTION_4_2_TABLES
            .iter()
            .filter(|(_, class)| *class == CaptureClass::Hooked)
            .map(|(name, _)| *name)
            .collect();
        let digested: std::collections::BTreeSet<&str> = REPLICATED_DIGEST_SPECS
            .iter()
            .map(|s| s.table_name)
            .collect();
        assert_eq!(
            hooked, digested,
            "every §4.2 replicated data table must have both an outbox hook and a QA digest spec"
        );
        // Control tables are present and intentionally not digested/hooked.
        assert!(SECTION_4_2_TABLES.contains(&("index_manifest", CaptureClass::ControlOnly)));
        assert!(SECTION_4_2_TABLES.contains(&("schema_migrations", CaptureClass::ControlOnly)));
    }

    #[test]
    fn scope_kind_tokens_are_stable() {
        assert_eq!(scope_kind::DOCUMENT, "document");
        assert_eq!(scope_kind::LEGI_METADATA_ROOT, "legi_metadata_root");
        assert_eq!(scope_kind::CITATION_RESOLUTION, "citation_resolution");
        assert_eq!(scope_kind::OFFICIAL_API_RESPONSE, "official_api_response");
    }

    #[test]
    fn outbox_event_scope_defaults_are_null() {
        let event = OutboxEvent::scope(
            "core",
            "documents",
            EventKind::Upsert,
            scope_kind::DOCUMENT,
            "legi:X@2020-01-01",
        );
        assert!(event.row_hash.is_none());
        assert!(event.payload.is_none());
        assert!(event.row_pk.is_none());
    }
}
