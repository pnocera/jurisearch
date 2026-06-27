//! Query-readiness: projection/embedding coverage metrics, the writer-owned readiness STAMP (work/09
//! P3A), and the legacy producer/`public` compute-on-read cache.
//!
//! On a client (installed) topology, `index_manifest['query_readiness']` is a **writer-owned stamp**:
//! the writer computes coverage over the new active generation and upserts it INSIDE the apply /
//! activation transaction ([`stamp_query_readiness`]), gated on complete coverage, and the read path is
//! a pure lookup ([`load_query_readiness`]) — a missing / stale / malformed value is a writer/apply
//! fault, NEVER a query-time recompute. For the `public` producer/local working set (which is never
//! activated via syncd), the same key remains a legacy compute-on-read cache
//! ([`load_or_compute_query_readiness`]). Both are scoped to the active read topology by
//! [`active_read_signature`] (`corpus:active_generation:sequence` per corpus, or `public`), so a value
//! computed against a different topology can never authorize the current one.

use super::*;
use crate::generations::schema_for_generation;
use crate::query::ReadSnapshot;
use crate::runtime::{sql_identifier, sql_string_literal};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestReadinessReport {
    pub projection_coverage: CoverageMetric,
    pub embedding_coverage: CoverageMetric,
}

/// The cached readiness report plus the read-topology signature it was computed against. A cache hit
/// is honoured only when the embedded `signature` still equals the current [`active_read_signature`],
/// so a generation switch (or a stale `public` report) forces a recompute rather than authorizing the
/// wrong tables.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedReadiness {
    signature: String,
    report: IngestReadinessReport,
}

/// A compact signature of the active read topology, used to scope the readiness cache. Each installed
/// corpus contributes `corpus:active_generation:sequence` (ordered by corpus); an empty `corpus_state`
/// (producer / fresh client) yields `public`. Same `corpus_state` → same signature, so producer-side
/// behaviour is unchanged (`public`).
fn active_read_signature<C: GenericClient>(client: &mut C) -> Result<String, StorageError> {
    let row = client
        .query_one(
            "SELECT coalesce( \
                 string_agg(corpus || ':' || active_generation || ':' || sequence::text, ',' \
                            ORDER BY corpus), \
                 'public') \
             FROM jurisearch_control.corpus_state;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(row.get(0))
}

/// Set `client`'s `search_path` to the client read role for the installed corpora — the active
/// generation's physical schema(s) then `public` — so the coverage queries measure the active
/// generation. Mirrors [`crate::runtime::ManagedPostgres::execute_read_sql`]: 0 corpora → `public`;
/// 1 → `jurisearch_server_<gen>, public`; >1 → `jurisearch_server, public`. Returns the read signature.
pub(super) fn apply_read_search_path<C: GenericClient>(
    client: &mut C,
) -> Result<String, StorageError> {
    let rows = client
        .query(
            "SELECT active_generation FROM jurisearch_control.corpus_state ORDER BY corpus;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let path = match rows.len() {
        0 => "public".to_owned(),
        1 => {
            let generation: String = rows[0].get("active_generation");
            format!(
                "{}, public",
                sql_identifier(&format!("jurisearch_server_{generation}"))
            )
        }
        _ => format!("{}, public", sql_identifier("jurisearch_server")),
    };
    client
        .batch_execute(&format!("SET search_path TO {path};"))
        .map_err(StorageError::PostgresClient)?;
    active_read_signature(client)
}

pub fn load_ingest_readiness(
    postgres: &ManagedPostgres,
) -> Result<IngestReadinessReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    apply_read_search_path(&mut client)?;
    load_readiness_metrics(&mut client)
}

pub fn load_ingest_projection_coverage(
    postgres: &ManagedPostgres,
) -> Result<CoverageMetric, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    apply_read_search_path(&mut client)?;
    load_projection_coverage(&mut client)
}

pub fn load_ingest_embedding_coverage(
    postgres: &ManagedPostgres,
) -> Result<CoverageMetric, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    apply_read_search_path(&mut client)?;
    load_embedding_coverage(&mut client)
}

pub(super) fn load_readiness_metrics(
    client: &mut postgres::Client,
) -> Result<IngestReadinessReport, StorageError> {
    Ok(IngestReadinessReport {
        projection_coverage: load_projection_coverage(client)?,
        embedding_coverage: load_embedding_coverage(client)?,
    })
}

/// Manifest key holding a cached, fully-ready query-readiness report. Its mere PRESENCE means the
/// index was fully query-ready (projection AND embedding coverage complete) at cache time; ingest
/// and embed runs delete it (see `invalidate_query_readiness`), so a present entry is still valid.
const QUERY_READINESS_MANIFEST_KEY: &str = "query_readiness";

/// Load the cached fully-ready query-readiness report, if present and parseable. A returned `Some`
/// means the index was fully query-ready and nothing has ingested/embedded since.
pub fn load_cached_query_readiness(
    postgres: &ManagedPostgres,
) -> Result<Option<IngestReadinessReport>, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let signature = active_read_signature(&mut client)?;
    let Some(row) = client
        .query_opt(
            "SELECT value::text FROM index_manifest WHERE key = $1;",
            &[&QUERY_READINESS_MANIFEST_KEY],
        )
        .map_err(StorageError::PostgresClient)?
    else {
        return Ok(None);
    };
    // Honour the cache only for the read topology it was computed against (a `public`/retired-gen
    // report must not authorize the current active generation).
    Ok(
        serde_json::from_str::<CachedReadiness>(&row.get::<_, String>(0))
            .ok()
            .filter(|cached| cached.signature == signature)
            .map(|cached| cached.report),
    )
}

/// Cache a fully-ready readiness report so subsequent query-readiness checks skip the full-corpus
/// coverage aggregations. Callers MUST only store a report whose projection AND embedding coverage
/// are complete, since the cache fast-path treats presence as "ready for every gate".
pub fn store_query_readiness(
    postgres: &ManagedPostgres,
    report: &IngestReadinessReport,
) -> Result<(), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let signature = active_read_signature(&mut client)?;
    let value = serde_json::to_string(&CachedReadiness {
        signature,
        report: report.clone(),
    })
    .map_err(StorageError::Json)?;
    client
        .execute(
            "INSERT INTO index_manifest(key, value, updated_at) \
             VALUES ($1, $2::text::jsonb, now()) \
             ON CONFLICT (key) DO UPDATE \
             SET value = EXCLUDED.value, \
                 updated_at = EXCLUDED.updated_at;",
            &[&QUERY_READINESS_MANIFEST_KEY, &value],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// Drop the cached readiness report so the next query-readiness check recomputes coverage live.
/// Called at the start of ingest and embed runs (which can change coverage).
pub fn invalidate_query_readiness<C: GenericClient>(client: &mut C) -> Result<(), StorageError> {
    client
        .execute(
            "DELETE FROM index_manifest WHERE key = $1;",
            &[&QUERY_READINESS_MANIFEST_KEY],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// Convenience wrapper over [`invalidate_query_readiness`] for callers that hold a `ManagedPostgres`
/// rather than a client (e.g. the embed-chunks command, which mutates `chunk_embeddings`).
pub fn invalidate_cached_query_readiness(postgres: &ManagedPostgres) -> Result<(), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    invalidate_query_readiness(&mut client)
}

/// Resolve the index's query-readiness report, preferring the manifest cache. On a cache hit the
/// returned `bool` is `true` and no coverage aggregation runs; on a miss the full projection and
/// embedding coverage are computed, and a fully-ready result is cached for next time. All of this
/// happens on ONE connection (a cache hit is a single indexed manifest lookup), so the common hot
/// path costs one round-trip instead of the full-corpus `count(DISTINCT)`/`count(*)` scans.
pub fn load_or_compute_query_readiness(
    postgres: &ManagedPostgres,
) -> Result<(IngestReadinessReport, bool), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    // Resolve the client read role: point `search_path` at the active generation (so coverage measures
    // the generation, not stale `public`) and obtain the signature that scopes the cache. `index_manifest`
    // is global and resolves through the `public` fallback regardless.
    let signature = apply_read_search_path(&mut client)?;

    if let Some(row) = client
        .query_opt(
            "SELECT value::text FROM index_manifest WHERE key = $1;",
            &[&QUERY_READINESS_MANIFEST_KEY],
        )
        .map_err(StorageError::PostgresClient)?
        && let Ok(cached) = serde_json::from_str::<CachedReadiness>(&row.get::<_, String>(0))
        && cached.signature == signature
    {
        return Ok((cached.report, true));
    }

    let report = IngestReadinessReport {
        projection_coverage: load_projection_coverage(&mut client)?,
        embedding_coverage: load_embedding_coverage(&mut client)?,
    };
    let fully_ready = coverage_is_complete(&report.projection_coverage)
        && coverage_is_complete(&report.embedding_coverage);
    if fully_ready {
        let value = serde_json::to_string(&CachedReadiness {
            signature,
            report: report.clone(),
        })
        .map_err(StorageError::Json)?;
        client
            .execute(
                "INSERT INTO index_manifest(key, value, updated_at) \
                 VALUES ($1, $2::text::jsonb, now()) \
                 ON CONFLICT (key) DO UPDATE \
                 SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at;",
                &[&QUERY_READINESS_MANIFEST_KEY, &value],
            )
            .map_err(StorageError::PostgresClient)?;
    }
    Ok((report, false))
}

/// A coverage metric is complete when every counted item is covered and at least one exists. (So an
/// EMPTY package/generation — `total == 0` — is never "ready" and cannot activate; that is intentional.)
fn coverage_is_complete(metric: &CoverageMetric) -> bool {
    metric.total > 0 && metric.covered == metric.total
}

// ----- work/09 P3A: writer-owned readiness STAMP (not a query-time cache) ----------------------------
//
// On a client (installed) topology, `index_manifest['query_readiness']` is a **writer-owned stamp**,
// not a read-time cache: the writer computes coverage over the new active generation and upserts it
// INSIDE the apply/activation transaction (gated on complete coverage), and the read path is a pure
// lookup — a missing/stale/malformed stamp is a writer/apply fault, never a query-time recompute.
// (For the `public` producer/local working set, the same key remains a legacy compute-on-read cache.)

/// Compute projection + embedding(dense) coverage over a SPECIFIC physical generation schema, with
/// schema-qualified queries (no `search_path`), so it can run inside the writer's apply transaction
/// (`&mut impl GenericClient`, including a `postgres::Transaction`).
///
/// Dense coverage is gated on the **active** generation fingerprint (`active_fingerprint`, from
/// `corpus_state.embedding_fingerprint`), NOT mere chunk↔embedding self-consistency: a chunk counts as
/// embedded only when BOTH `chunks.embedding_fingerprint` AND its `chunk_embeddings.embedding_fingerprint`
/// equal `active_fingerprint`. Otherwise a generation whose rows are internally consistent with an OLD
/// fingerprint could be stamped ready while dense retrieval — which filters `chunk_embeddings` by the
/// active fingerprint — finds zero vectors (silent lexical fallback / false no-results), exactly the
/// failure P3A closes.
fn compute_generation_coverage<C: GenericClient>(
    client: &mut C,
    schema: &str,
    active_fingerprint: &str,
) -> Result<IngestReadinessReport, StorageError> {
    let schema = sql_identifier(schema);
    let active = sql_string_literal(active_fingerprint);
    let projection = client
        .query_one(
            &format!(
                "SELECT count(DISTINCT d.document_id)::bigint, \
                        count(DISTINCT d.document_id) FILTER (WHERE c.chunk_id IS NOT NULL)::bigint \
                 FROM {schema}.documents d \
                 LEFT JOIN {schema}.chunks c ON c.document_id = d.document_id;"
            ),
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let total_documents: i64 = projection.get(0);
    let projected_documents: i64 = projection.get(1);

    let embedding = client
        .query_one(
            &format!(
                "SELECT count(*)::bigint, \
                        count(*) FILTER ( \
                            WHERE c.embedding_fingerprint = {active} \
                              AND ce.chunk_id IS NOT NULL \
                              AND ce.embedding_fingerprint = {active} \
                        )::bigint \
                 FROM {schema}.chunks c \
                 LEFT JOIN {schema}.chunk_embeddings ce ON ce.chunk_id = c.chunk_id;"
            ),
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let total_chunks: i64 = embedding.get(0);
    let embedded_chunks: i64 = embedding.get(1);

    Ok(IngestReadinessReport {
        projection_coverage: CoverageMetric {
            covered: projected_documents,
            total: total_documents,
            percentage: percentage(projected_documents, total_documents),
        },
        embedding_coverage: CoverageMetric {
            covered: embedded_chunks,
            total: total_chunks,
            percentage: percentage(embedded_chunks, total_chunks),
        },
    })
}

/// Writer-owned readiness stamp (work/09 P3A): compute projection + dense coverage over the SPECIFIC
/// generation being activated/mutated (`generation`, e.g. `core_g0002`) and upsert the
/// `query_readiness` stamp with the post-switch signature, **inside the caller's apply transaction**.
/// INCOMPLETE projection or embedding coverage (or a stamp write failure) returns an error, so the
/// caller's transaction rolls back and the cursor never advances onto a not-ready topology.
///
/// MUST be called after the switch has updated `corpus_state` (so `active_read_signature` is the new
/// topology) and before the transaction commits.
///
/// 3A is single-corpus. The stamp's signature is the aggregate `active_read_signature`, but the
/// coverage measured here is that of the ONE generation passed in. With more than one active corpus
/// the stamp therefore reflects only the just-applied corpus, not aggregate readiness — that aggregate
/// coverage (and the multi-corpus READ that would need it) is deferred to 3C (multi-corpus fan-out).
pub fn stamp_query_readiness<C: GenericClient>(
    client: &mut C,
    generation: &str,
) -> Result<IngestReadinessReport, StorageError> {
    let signature = active_read_signature(client)?;
    // The ACTIVE fingerprint this generation was activated for — dense coverage must match it, not just
    // be internally self-consistent.
    let active_fingerprint: String = client
        .query_opt(
            "SELECT embedding_fingerprint FROM jurisearch_control.corpus_state \
             WHERE active_generation = $1;",
            &[&generation],
        )
        .map_err(StorageError::PostgresClient)?
        .ok_or_else(|| StorageError::IngestAccounting {
            message: format!(
                "stamp_query_readiness: no active corpus_state row for generation `{generation}`"
            ),
        })?
        .get(0);
    let schema = schema_for_generation(generation);
    let report = compute_generation_coverage(client, &schema, &active_fingerprint)?;
    if !coverage_is_complete(&report.projection_coverage) {
        return Err(StorageError::IngestAccounting {
            message: format!(
                "incomplete projection coverage for `{schema}` ({} / {}); generation not query-ready",
                report.projection_coverage.covered, report.projection_coverage.total
            ),
        });
    }
    if !coverage_is_complete(&report.embedding_coverage) {
        return Err(StorageError::IngestAccounting {
            message: format!(
                "incomplete dense coverage for `{schema}` ({} / {}); generation not query-ready",
                report.embedding_coverage.covered, report.embedding_coverage.total
            ),
        });
    }
    let value = serde_json::to_string(&CachedReadiness {
        signature,
        report: report.clone(),
    })
    .map_err(StorageError::Json)?;
    // `public.index_manifest` is schema-qualified: the incremental apply runs this stamp under a
    // `search_path` pointed at the active GENERATION schema, where an unqualified `index_manifest`
    // would not resolve.
    client
        .execute(
            "INSERT INTO public.index_manifest(key, value, updated_at) \
             VALUES ($1, $2::text::jsonb, now()) \
             ON CONFLICT (key) DO UPDATE \
             SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at;",
            &[&QUERY_READINESS_MANIFEST_KEY, &value],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(report)
}

/// Read-only readiness LOOKUP for an installed (client) topology — the work/09 site read path. NEVER
/// computes or writes: a `public` topology (no active corpus) is "index unavailable"; an installed
/// topology with a missing / stale / malformed stamp is a writer/apply fault (the writer must have
/// stamped readiness at apply time). Safe under a SELECT-only read role.
pub fn load_query_readiness_with_client<C: GenericClient>(
    client: &mut C,
) -> Result<IngestReadinessReport, StorageError> {
    let signature = active_read_signature(client)?;
    if signature == "public" {
        return Err(StorageError::IngestAccounting {
            message: "no active corpus installed; index unavailable".to_owned(),
        });
    }
    // Fail CLOSED on multi-corpus (work/09 P3A is single-corpus): the writer-owned stamp records the
    // aggregate topology signature but only the just-applied corpus's coverage, so it cannot prove
    // aggregate readiness over the `jurisearch_server` union views. A multi-corpus read must error
    // rather than authorize fetch/BM25 over a single-corpus report (multi-corpus is deferred to 3C).
    let active_corpora: i64 = client
        .query_one(
            "SELECT count(*)::bigint FROM jurisearch_control.corpus_state;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    if active_corpora > 1 {
        return Err(StorageError::IngestAccounting {
            message:
                "multi-corpus query readiness is not supported until 3C (multi-corpus fan-out); \
                      this build serves a single active corpus"
                    .to_owned(),
        });
    }
    let Some(row) = client
        .query_opt(
            "SELECT value::text FROM public.index_manifest WHERE key = $1;",
            &[&QUERY_READINESS_MANIFEST_KEY],
        )
        .map_err(StorageError::PostgresClient)?
    else {
        return Err(StorageError::IngestAccounting {
            message: format!(
                "query readiness was never stamped for the active topology `{signature}` \
                 (writer/apply fault)"
            ),
        });
    };
    let cached: CachedReadiness =
        serde_json::from_str(&row.get::<_, String>(0)).map_err(|error| {
            StorageError::IngestAccounting {
                message: format!("malformed query_readiness stamp: {error}"),
            }
        })?;
    if cached.signature != signature {
        return Err(StorageError::IngestAccounting {
            message: format!(
                "stale query readiness stamp: stamped for `{}` but the active topology is `{signature}` \
                 (writer/apply fault — the topology changed without restamping)",
                cached.signature
            ),
        });
    }
    Ok(cached.report)
}

/// Read-only readiness lookup over a fresh connection (site read path).
pub fn load_query_readiness(
    postgres: &ManagedPostgres,
) -> Result<IngestReadinessReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    load_query_readiness_with_client(&mut client)
}

/// Read-only readiness LOOKUP bound to an open read SNAPSHOT (work/09 P4 site query service): the
/// site handlers open ONE snapshot per request, validate the writer-owned `query_readiness` stamp on
/// that same snapshot BEFORE running a builder, and never recompute coverage or write. It is the exact
/// snapshot analogue of [`load_query_readiness_with_client`] — a `public` topology (no active corpus) is
/// "index unavailable"; a missing / stale / malformed stamp is a writer/apply fault — but it derives the
/// active-topology signature from the snapshot's already-resolved `active_corpora()` (the routing
/// authority) so the stamp is validated against the SAME topology the request's reads will use, with no
/// second connection and no TOCTOU against the request snapshot.
///
/// Multi-corpus is fail-closed (work/09 P3A/P3C): the writer stamp records the aggregate signature but
/// only the just-applied corpus's coverage, so it cannot prove aggregate readiness; a >1-corpus site
/// request errors here rather than being served on a single-corpus stamp (health reports the gap).
pub fn load_query_readiness_in_snapshot(
    snapshot: &mut dyn ReadSnapshot,
) -> Result<IngestReadinessReport, StorageError> {
    // Signature over the active read topology, derived from the snapshot's resolved corpora. This is
    // byte-identical to `active_read_signature`'s `string_agg(corpus || ':' || active_generation || ':'
    // || sequence ORDER BY corpus)` because `resolve_active_corpora` orders by corpus and carries the
    // same generation/sequence values — so it compares equal to the writer's stamped signature.
    let (signature, active_count) = {
        let corpora = snapshot.active_corpora();
        let signature = corpora
            .iter()
            .map(|corpus| {
                format!(
                    "{}:{}:{}",
                    corpus.corpus, corpus.generation, corpus.sequence
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        (signature, corpora.len())
    };
    if active_count == 0 {
        return Err(StorageError::IngestAccounting {
            message: "no active corpus installed; index unavailable".to_owned(),
        });
    }
    if active_count > 1 {
        return Err(StorageError::IngestAccounting {
            message:
                "multi-corpus query readiness is not supported until 3C (multi-corpus fan-out); \
                      this build serves a single active corpus"
                    .to_owned(),
        });
    }
    let stamp = snapshot.read_text(
        "SELECT value::text FROM public.index_manifest WHERE key = 'query_readiness';",
    )?;
    if stamp.is_empty() {
        return Err(StorageError::IngestAccounting {
            message: format!(
                "query readiness was never stamped for the active topology `{signature}` \
                 (writer/apply fault)"
            ),
        });
    }
    let cached: CachedReadiness =
        serde_json::from_str(&stamp).map_err(|error| StorageError::IngestAccounting {
            message: format!("malformed query_readiness stamp: {error}"),
        })?;
    if cached.signature != signature {
        return Err(StorageError::IngestAccounting {
            message: format!(
                "stale query readiness stamp: stamped for `{}` but the active topology is `{signature}` \
                 (writer/apply fault — the topology changed without restamping)",
                cached.signature
            ),
        });
    }
    Ok(cached.report)
}

/// The LOCAL read gate's readiness resolution (work/09 P3A): an installed (client) topology is a
/// strict writer-owned-stamp LOOKUP (never recomputes); the `public` producer/local working set keeps
/// the legacy compute-on-read cache. (The site query service uses [`load_query_readiness`] directly.)
pub fn resolve_query_readiness(
    postgres: &ManagedPostgres,
) -> Result<IngestReadinessReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    if active_read_signature(&mut client)? == "public" {
        // Legacy producer/local working set: compute coverage on read (and cache it).
        let (report, _from_cache) = load_or_compute_query_readiness(postgres)?;
        Ok(report)
    } else {
        load_query_readiness_with_client(&mut client)
    }
}

fn load_projection_coverage<C: GenericClient>(
    client: &mut C,
) -> Result<CoverageMetric, StorageError> {
    let projection = client
        .query_one(
            "SELECT count(DISTINCT d.document_id)::bigint, \
                    count(DISTINCT d.document_id) FILTER (WHERE c.chunk_id IS NOT NULL)::bigint \
             FROM documents d \
             LEFT JOIN chunks c ON c.document_id = d.document_id;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let total_documents: i64 = projection.get(0);
    let projected_documents: i64 = projection.get(1);

    Ok(CoverageMetric {
        covered: projected_documents,
        total: total_documents,
        percentage: percentage(projected_documents, total_documents),
    })
}

fn load_embedding_coverage<C: GenericClient>(
    client: &mut C,
) -> Result<CoverageMetric, StorageError> {
    // The non-NULL guards are redundant with SQL equality semantics, but make
    // the freshness requirement explicit in the coverage query.
    let embedding = client
        .query_one(
            "SELECT count(*)::bigint, \
                    count(*) FILTER ( \
                        WHERE c.embedding_fingerprint IS NOT NULL \
                          AND ce.chunk_id IS NOT NULL \
                          AND ce.embedding_fingerprint = c.embedding_fingerprint \
                    )::bigint \
             FROM chunks c \
             LEFT JOIN chunk_embeddings ce ON ce.chunk_id = c.chunk_id;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    let total_chunks: i64 = embedding.get(0);
    let embedded_chunks: i64 = embedding.get(1);

    Ok(CoverageMetric {
        covered: embedded_chunks,
        total: total_chunks,
        percentage: percentage(embedded_chunks, total_chunks),
    })
}
