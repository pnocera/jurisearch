//! work/09 P3B/P3C — the read-only-safe, snapshot-consistent read path.
//!
//! A query is **one request = one read snapshot**: a single transaction that resolves the active
//! corpora ONCE (the routing authority) and runs every read of that request against that one MVCC
//! snapshot. Because the operated activation switch is a `CREATE OR REPLACE VIEW` that never drops the
//! previously-active physical generation ([`crate::generations`]), a snapshot stays valid even if a swap
//! commits mid-request — so an in-flight query observes a single, coherent topology (a swap is invisible
//! to it), and the next request opens a fresh snapshot on the new topology.
//!
//! The resolver returns every active corpus. 0 corpora is the `public` producer/local working set; one
//! corpus pins that generation; more than one is the 3C multi-corpus topology. Each read sets the
//! `search_path` it needs ([`ReadSnapshot::read_text`] uses the request default — the union views for a
//! multi-corpus topology; [`ReadSnapshot::read_text_for_corpus`] targets one physical generation for the
//! hot-search fan-out), so an arm never poisons a later default read.

use postgres::{GenericClient, SimpleQueryMessage};

use crate::generations::SERVER_VIEW_SCHEMA;
use crate::runtime::{ManagedPostgres, StorageError, sql_identifier};

/// One active corpus, resolved from `jurisearch_control.corpus_state` — the SINGLE authority mapping a
/// corpus to its active physical generation schema. Both the read snapshot (to route retrieval) and the
/// writer (to stamp readiness for the active topology) resolve through this; it is never re-derived ad
/// hoc (this replaces the duplicated `corpus_state` resolution in `execute_read_sql`,
/// `apply_read_search_path`, and the hybrid fingerprint preflight).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveCorpus {
    /// The corpus id, e.g. `core`.
    pub corpus: String,
    /// The active generation label, e.g. `core_g0001`.
    pub generation: String,
    /// The physical generation schema, e.g. `jurisearch_server_core_g0001` — where the BM25/IVFFlat
    /// indexes live (hot search hits this, never the union views).
    pub schema: String,
    /// The cursor sequence the active generation is at.
    pub sequence: i64,
    /// The active generation's embedding fingerprint (the dense-retrieval compatibility key).
    pub fingerprint: String,
}

/// Resolve every active corpus from `jurisearch_control.corpus_state` (ordered by corpus, so the read
/// topology is deterministic). The empty result is the `public` producer/local working set.
pub fn resolve_active_corpora<C: GenericClient>(
    client: &mut C,
) -> Result<Vec<ActiveCorpus>, StorageError> {
    let rows = client
        .query(
            "SELECT corpus, active_generation, sequence, embedding_fingerprint \
             FROM jurisearch_control.corpus_state ORDER BY corpus;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(rows
        .iter()
        .map(|row| {
            let corpus: String = row.get("corpus");
            let generation: String = row.get("active_generation");
            let schema = format!("{SERVER_VIEW_SCHEMA}_{generation}");
            ActiveCorpus {
                corpus,
                generation,
                schema,
                sequence: row.get("sequence"),
                fingerprint: row.get("embedding_fingerprint"),
            }
        })
        .collect())
}

/// The read `search_path` for a single active topology — mirrors the legacy `execute_read_sql` resolver:
/// 0 corpora → `public` (producer/local); 1 → `<physical generation>, public` so index scans hit the
/// indexed generation tables. (A query snapshot never opens over >1 corpus in 3B.)
fn read_search_path(corpora: &[ActiveCorpus]) -> String {
    match corpora {
        [] => "public".to_owned(),
        [one] => format!("{}, public", sql_identifier(&one.schema)),
        // Unreachable on the query-snapshot path (the snapshot refuses >1); kept total for clarity.
        _ => format!("{}, public", sql_identifier(SERVER_VIEW_SCHEMA)),
    }
}

/// A single read snapshot: every read of a request runs through this handle, against one MVCC snapshot
/// and one pinned `search_path`. Object-safe (held as `&mut dyn ReadSnapshot` by the response builders),
/// with `&mut self` read access so the snapshot owns its connection without interior mutability.
pub trait ReadSnapshot {
    /// Run read SQL in the held snapshot transaction against the pinned `search_path`, returning the
    /// result text with `psql -qAt` semantics — unaligned, tuples-only, columns joined by `|`, rows by
    /// newline, SQL `NULL` rendered as the empty string, the whole output trimmed. This is the drop-in
    /// for the legacy [`ManagedPostgres::execute_read_sql`], so the storage read SQL (and therefore the
    /// JSON it returns) is byte-identical.
    fn read_text(&mut self, sql: &str) -> Result<String, StorageError>;

    /// Run read SQL THROUGH a single corpus's active physical generation (`<schema>, public`), for the
    /// multi-corpus fan-out (work/09 3C): each arm runs the existing per-operation SQL against one
    /// physical generation's BM25/IVFFlat indexes, then the caller fuses above the arms. The path is set
    /// per call, so an arm can never leave a later [`read_text`] resolving against the wrong corpus.
    fn read_text_for_corpus(
        &mut self,
        corpus: &ActiveCorpus,
        sql: &str,
    ) -> Result<String, StorageError>;

    /// The active corpora resolved ONCE at snapshot open — the routing authority (and the source of the
    /// dense-retrieval fingerprint). Empty for the `public` producer/local working set; one entry for a
    /// single-corpus topology; more than one for the 3C multi-corpus fan-out.
    fn active_corpora(&self) -> &[ActiveCorpus];
}

/// A read-only store that hands out one [`ReadSnapshot`] per request (the read role, ISP-disjoint from
/// the writer). Object-safe so the dispatcher (P4) can hold it as `&dyn QueryStore`.
pub trait QueryStore {
    /// Open ONE read snapshot; all of a request's reads run through it, and it ends (rolls back, it is
    /// read-only) when the handle is dropped. Its lifetime is bounded by `&self`.
    fn begin_snapshot(&self) -> Result<Box<dyn ReadSnapshot + '_>, StorageError>;
}

/// The local self-managed snapshot: owns a dedicated libpq connection running one
/// `REPEATABLE READ READ ONLY` transaction. Each read sets its own `search_path` (the request default
/// for [`read_text`](ReadSnapshot::read_text), a single physical generation for
/// [`read_text_for_corpus`](ReadSnapshot::read_text_for_corpus)), so a fan-out arm can never poison a
/// later default read.
pub struct LocalSnapshot {
    client: postgres::Client,
    corpora: Vec<ActiveCorpus>,
    /// The request's DEFAULT read path: 0 corpora → `public`; 1 → that generation + `public`; >1 →
    /// `jurisearch_server, public` (the union views — correct for by-id / non-indexed reads; hot indexed
    /// multi-corpus search uses the per-corpus path via [`read_text_for_corpus`]).
    default_path: String,
}

impl LocalSnapshot {
    /// Open the snapshot on `client`: begin a `REPEATABLE READ READ ONLY` transaction and resolve the
    /// active corpora (this first query establishes the MVCC snapshot deterministically). The default
    /// read path is computed but NOT set here — each read sets the path it needs.
    fn open(mut client: postgres::Client) -> Result<Self, StorageError> {
        client
            .batch_execute("BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;")
            .map_err(StorageError::PostgresClient)?;
        let corpora = resolve_active_corpora(&mut client)?;
        let default_path = read_search_path(&corpora);
        Ok(Self {
            client,
            corpora,
            default_path,
        })
    }
}

impl ReadSnapshot for LocalSnapshot {
    fn read_text(&mut self, sql: &str) -> Result<String, StorageError> {
        let combined = format!("SET LOCAL search_path TO {};\n{sql}", self.default_path);
        simple_query_text(&mut self.client, &combined)
    }

    fn read_text_for_corpus(
        &mut self,
        corpus: &ActiveCorpus,
        sql: &str,
    ) -> Result<String, StorageError> {
        let path = format!("{}, public", sql_identifier(&corpus.schema));
        let combined = format!("SET LOCAL search_path TO {path};\n{sql}");
        simple_query_text(&mut self.client, &combined)
    }

    fn active_corpora(&self) -> &[ActiveCorpus] {
        &self.corpora
    }
}

impl Drop for LocalSnapshot {
    fn drop(&mut self) {
        // Read-only: rollback is the safe best-effort close (equivalent to commit for state, but never
        // surfaces an error from Drop).
        let _ = self.client.batch_execute("ROLLBACK;");
    }
}

impl QueryStore for ManagedPostgres {
    fn begin_snapshot(&self) -> Result<Box<dyn ReadSnapshot + '_>, StorageError> {
        Ok(Box::new(LocalSnapshot::open(self.client()?)?))
    }
}

/// Run `sql` (one or more statements) and render the result with `psql -qAt` semantics: every result
/// row's columns joined by `|`, rows joined by newline, `NULL` as the empty string, the whole output
/// trimmed. Statements that return no rows (e.g. a leading `SET ivfflat.probes = …`) contribute
/// nothing, exactly as `psql -c` would print.
fn simple_query_text(client: &mut postgres::Client, sql: &str) -> Result<String, StorageError> {
    let messages = client
        .simple_query(sql)
        .map_err(StorageError::PostgresClient)?;
    let mut lines: Vec<String> = Vec::new();
    for message in messages {
        if let SimpleQueryMessage::Row(row) = message {
            let columns = row.columns().len();
            let rendered = (0..columns)
                .map(|index| row.get(index).unwrap_or("").to_owned())
                .collect::<Vec<_>>()
                .join("|");
            lines.push(rendered);
        }
    }
    Ok(lines.join("\n").trim().to_owned())
}
