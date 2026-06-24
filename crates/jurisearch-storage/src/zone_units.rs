//! Option B parallel zone-retrieval subsystem storage (migrations v13–v15).
//!
//! Official Judilibre zone fragments are materialized as first-class retrieval units in `zone_units`
//! (+ `zone_unit_embeddings` dense space + `zone_units_bm25_idx` lexical space), SEPARATE from the bulk
//! `chunks` corpus so the proven whole-decision retrieval path and the Phase 2 `zone_accurate=false`
//! honesty invariant stay untouched. This module owns the data plumbing: the enrichment candidate set,
//! the `decision_zones` → `zone_units` derivation write, and the zone-unit dense embed/finalize (copies
//! of the `dense.rs` / `projection.rs` shapes pointed at the zone tables, so the chunk paths are
//! provably unchanged). Reads go through `execute_sql` (JSON, like the other read helpers); writes use a
//! parameterized client (like the ingestion writes).

use crate::dense::DenseRebuildSpec;
use crate::runtime::{ManagedPostgres, StorageError, sql_string_literal};

/// Name of the zone-unit dense ANN index (built at finalize time, like the chunk ivfflat index).
pub const ZONE_UNIT_VECTOR_INDEX_NAME: &str = "zone_unit_embeddings_ivfflat_idx";
/// Sources Judilibre resolves by pourvoi+date (Cour de cassation: published + inédit). Mirrors the CLI
/// `is_judilibre_cassation_source`; the only sources that can ever carry official zones.
pub const ZONE_ENRICHABLE_SOURCES: [&str; 2] = ["cass", "inca"];

/// A parser-valid pourvoi (`NN-NNNN..`) exists among the decision's normalized case numbers — the same
/// reachability gate as `decision_resolution_metadata_json`, expressed against the GIN-indexed
/// `jurisearch_normalized_case_numbers` function (migration v11).
const PARSER_VALID_POURVOI_EXISTS: &str = "EXISTS (SELECT 1 FROM \
     unnest(jurisearch_normalized_case_numbers(d.canonical_json)) AS cn \
     WHERE cn ~ '^[0-9]{2}-[0-9]{4,6}$')";

/// Direction the enrichment backfill walks the resolver-reachable candidate set. `document_id` order is
/// chronological (JURITEXT ids are issued over time), and official Judilibre zones exist only for recent
/// decisions — so `Recent` (newest first) reaches the zoned decisions immediately, while the default
/// `Oldest` preserves the original keyset for compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrichZoneOrder {
    Oldest,
    Recent,
}

/// Page the decisions that need enrichment for `source` (one of [`ZONE_ENRICHABLE_SOURCES`]): Cassation
/// decisions with a parser-valid pourvoi whose `decision_zones` row is missing, expired, OR a fresh
/// `ok`/`invalid_offsets` row whose `text_hash IS NULL` (the lazy/pre-hash-fix rows — re-enriched
/// regardless of TTL so they become derivable). When `since` is set, also re-enrich anything cached
/// before that instant (refresh). Keyset paging on `document_id` via `after_cursor` (exclusive), walked
/// in `order` direction — `Recent` pages newest→oldest (`document_id < cursor`), `Oldest` the reverse.
/// Returns `{ "candidates": [{document_id, source}], "next_cursor": <boundary id|null> }` where the
/// boundary is the min (Recent) or max (Oldest) `document_id` of the page, to feed the next page.
pub fn enrich_zone_candidates_json(
    postgres: &ManagedPostgres,
    source: &str,
    after_cursor: Option<&str>,
    since: Option<&str>,
    limit: u32,
    order: EnrichZoneOrder,
) -> Result<String, StorageError> {
    // Keyset comparison / sort / boundary aggregate per walk direction.
    let (cursor_cmp, sort_dir, boundary_agg) = match order {
        EnrichZoneOrder::Oldest => (">", "ASC", "max"),
        EnrichZoneOrder::Recent => ("<", "DESC", "min"),
    };
    let source_literal = sql_string_literal(source);
    let cursor_predicate = after_cursor
        .map(|cursor| format!("AND d.document_id {cursor_cmp} {}", sql_string_literal(cursor)))
        .unwrap_or_default();
    let since_predicate = since
        .map(|since| {
            format!(
                "OR z.fetched_at < {}::timestamptz",
                sql_string_literal(since)
            )
        })
        .unwrap_or_default();
    let limit = limit.max(1);
    postgres.execute_sql(&format!(
        r#"
WITH page AS (
    SELECT d.document_id, d.source
    FROM documents d
    LEFT JOIN decision_zones z ON z.document_id = d.document_id
    WHERE d.kind = 'decision'
      AND d.source = {source_literal}
      AND {PARSER_VALID_POURVOI_EXISTS}
      AND (
          z.status IS NULL
          OR z.expires_at <= now()
          OR (z.status IN ('ok','invalid_offsets') AND z.text_hash IS NULL)
          {since_predicate}
      )
      {cursor_predicate}
    ORDER BY d.document_id {sort_dir}
    LIMIT {limit}
)
SELECT jsonb_build_object(
    'candidates', COALESCE((
        SELECT jsonb_agg(jsonb_build_object('document_id', document_id, 'source', source)
                         ORDER BY document_id {sort_dir})
        FROM page
    ), '[]'::jsonb),
    'next_cursor', (SELECT {boundary_agg}(document_id) FROM page)
)::text;
"#
    ))
}

/// One derived zone-unit row to write. `zone_unit_id` is computed as `<document_id>#<zone>#<fragment>`.
pub struct ZoneUnitRow<'a> {
    pub document_id: &'a str,
    pub zone: &'a str,
    pub fragment_index: i32,
    pub body: &'a str,
    pub search_body: &'a str,
    pub source: &'a str,
    pub text_hash: &'a str,
    pub builder_version: &'a str,
}

/// Replace ALL of a decision's `zone_units` with `rows`, in one transaction (idempotent derivation: a
/// re-derive deletes the decision's prior units and reinserts the current set). An empty `rows` just
/// clears them (e.g. a decision whose zones became empty). Embeddings cascade-delete with their units.
pub fn replace_zone_units_for_document(
    postgres: &ManagedPostgres,
    document_id: &str,
    rows: &[ZoneUnitRow<'_>],
) -> Result<(), StorageError> {
    // Defensive: every row must belong to the document being replaced — otherwise a caller bug could
    // clear document A's units and insert document B's in one transaction (the units' own UNIQUE is on
    // (document_id, zone, fragment_index), which would not catch a foreign document_id).
    if let Some(foreign) = rows.iter().find(|row| row.document_id != document_id) {
        return Err(StorageError::Projection {
            message: format!(
                "replace_zone_units_for_document: row for `{}` does not match document `{document_id}`",
                foreign.document_id
            ),
        });
    }
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    transaction
        .execute(
            "DELETE FROM zone_units WHERE document_id = $1;",
            &[&document_id],
        )
        .map_err(StorageError::PostgresClient)?;

    if !rows.is_empty() {
        let ids: Vec<String> = rows
            .iter()
            .map(|row| format!("{}#{}#{}", row.document_id, row.zone, row.fragment_index))
            .collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let document_ids: Vec<&str> = rows.iter().map(|row| row.document_id).collect();
        let zones: Vec<&str> = rows.iter().map(|row| row.zone).collect();
        let fragments: Vec<i32> = rows.iter().map(|row| row.fragment_index).collect();
        let bodies: Vec<&str> = rows.iter().map(|row| row.body).collect();
        let search_bodies: Vec<&str> = rows.iter().map(|row| row.search_body).collect();
        let sources: Vec<&str> = rows.iter().map(|row| row.source).collect();
        let hashes: Vec<&str> = rows.iter().map(|row| row.text_hash).collect();
        let builder_versions: Vec<&str> = rows.iter().map(|row| row.builder_version).collect();
        transaction
            .execute(
                "INSERT INTO zone_units \
                    (zone_unit_id, document_id, zone, fragment_index, body, search_body, source, \
                     text_hash, zone_unit_builder_version) \
                 SELECT * FROM unnest($1::text[], $2::text[], $3::text[], $4::int[], $5::text[], \
                                      $6::text[], $7::text[], $8::text[], $9::text[]);",
                &[
                    &ids,
                    &document_ids,
                    &zones,
                    &fragments,
                    &bodies,
                    &search_bodies,
                    &sources,
                    &hashes,
                    &builder_versions,
                ],
            )
            .map_err(StorageError::PostgresClient)?;
    }
    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// Page the decisions whose `zone_units` need (re)deriving: a NON-EXPIRED `ok` `decision_zones` row
/// with a non-null `text_hash` (the BLOCKER-1 invariant — NULL-hash rows are re-enriched first, never
/// derived) for a resolver-reachable Cour de cassation decision (kind=decision, source cass/inca,
/// parser-valid pourvoi — the same scope as enrichment, so a stray/foreign `ok` row can never become
/// units), that either has no units yet OR whose units' `text_hash`/`zone_unit_builder_version` are
/// stale vs the row and the current `builder_version`. Expired rows are left to the refresh pass (never
/// derived stale). `rebuild=true` re-derives every eligible `ok` row regardless of unit state. Keyset
/// paging on `document_id`. Returns
/// `{ "candidates": [{document_id, source, text_hash, zones}], "next_cursor": <last id|null> }` where
/// `zones` is the `decision_zones.zones_json` object (the CLI parses fragments from it).
pub fn load_derivable_decision_zones_json(
    postgres: &ManagedPostgres,
    builder_version: &str,
    rebuild: bool,
    after_cursor: Option<&str>,
    limit: u32,
) -> Result<String, StorageError> {
    let builder_literal = sql_string_literal(builder_version);
    let cursor_predicate = after_cursor
        .map(|cursor| format!("AND z.document_id > {}", sql_string_literal(cursor)))
        .unwrap_or_default();
    let staleness_predicate = if rebuild {
        String::new()
    } else {
        format!(
            "AND (
                NOT EXISTS (SELECT 1 FROM zone_units u WHERE u.document_id = z.document_id)
                OR EXISTS (SELECT 1 FROM zone_units u WHERE u.document_id = z.document_id
                    AND (u.text_hash <> z.text_hash OR u.zone_unit_builder_version <> {builder_literal}))
            )"
        )
    };
    let limit = limit.max(1);
    postgres.execute_sql(&format!(
        r#"
WITH page AS (
    SELECT z.document_id, d.source, z.text_hash, z.zones_json
    FROM decision_zones z
    JOIN documents d ON d.document_id = z.document_id
    WHERE z.status = 'ok'
      AND z.text_hash IS NOT NULL
      AND (z.expires_at IS NULL OR z.expires_at > now())
      AND d.kind = 'decision'
      AND d.source IN ('cass','inca')
      AND {PARSER_VALID_POURVOI_EXISTS}
      {staleness_predicate}
      {cursor_predicate}
    ORDER BY z.document_id
    LIMIT {limit}
)
SELECT jsonb_build_object(
    'candidates', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'document_id', document_id,
            'source', source,
            'text_hash', text_hash,
            'zones', zones_json
        ) ORDER BY document_id)
        FROM page
    ), '[]'::jsonb),
    'next_cursor', (SELECT max(document_id) FROM page)
)::text;
"#
    ))
}

/// A zone unit to embed: its id and the text to embed (the zone fragment `body`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneUnitEmbeddingInput {
    pub zone_unit_id: String,
    pub embedding_text: String,
}

/// One zone-unit embedding to upsert (the literal is a pgvector text literal).
#[derive(Debug, Clone, Copy)]
pub struct ZoneUnitEmbeddingInsert<'a> {
    pub zone_unit_id: &'a str,
    pub embedding_fingerprint: &'a str,
    pub embedding_literal: &'a str,
    pub model: &'a str,
    pub dimension: usize,
}

/// Zone-unit equivalent of `load_chunk_embedding_inputs`: the next page of zone units that lack an
/// embedding under `(fingerprint, model, dimension)` (missing or drifted). Stable order for resumable
/// paging.
pub fn load_zone_unit_embedding_inputs(
    postgres: &ManagedPostgres,
    embedding_fingerprint: &str,
    model: &str,
    dimension: i32,
    limit: Option<u32>,
) -> Result<Vec<ZoneUnitEmbeddingInput>, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let base = "SELECT u.zone_unit_id, u.body \
                FROM zone_units u \
                LEFT JOIN zone_unit_embeddings e ON e.zone_unit_id = u.zone_unit_id \
                WHERE e.zone_unit_id IS NULL \
                   OR e.embedding_fingerprint <> $1 \
                   OR e.model <> $2 \
                   OR e.dimension <> $3 \
                ORDER BY u.document_id, u.zone, u.fragment_index, u.zone_unit_id";
    let rows = if let Some(limit) = limit {
        let limit = i64::from(limit);
        client
            .query(
                &format!("{base} LIMIT $4;"),
                &[&embedding_fingerprint, &model, &dimension, &limit],
            )
            .map_err(StorageError::PostgresClient)?
    } else {
        client
            .query(
                &format!("{base};"),
                &[&embedding_fingerprint, &model, &dimension],
            )
            .map_err(StorageError::PostgresClient)?
    };
    Ok(rows
        .into_iter()
        .map(|row| ZoneUnitEmbeddingInput {
            zone_unit_id: row.get(0),
            embedding_text: row.get(1),
        })
        .collect())
}

/// Zone-unit equivalent of `insert_chunk_embeddings`: batch-upsert zone-unit embeddings and stamp
/// `zone_units.embedding_fingerprint`. Mirrors the chunk writer's missing/conflicting-unit guard (a
/// short update count surfaces a concrete offender) rather than silently skipping bad staged ids.
pub fn insert_zone_unit_embeddings(
    postgres: &ManagedPostgres,
    embeddings: &[ZoneUnitEmbeddingInsert<'_>],
) -> Result<usize, StorageError> {
    if embeddings.is_empty() {
        return Ok(0);
    }
    let ids: Vec<&str> = embeddings.iter().map(|e| e.zone_unit_id).collect();
    let fingerprints: Vec<&str> = embeddings.iter().map(|e| e.embedding_fingerprint).collect();
    let literals: Vec<&str> = embeddings.iter().map(|e| e.embedding_literal).collect();
    let models: Vec<&str> = embeddings.iter().map(|e| e.model).collect();
    let dimensions: Vec<i32> = embeddings
        .iter()
        .map(|e| {
            i32::try_from(e.dimension).map_err(|_| StorageError::Projection {
                message: format!(
                    "embedding dimension too large for storage on zone unit `{}`: {}",
                    e.zone_unit_id, e.dimension
                ),
            })
        })
        .collect::<Result<_, _>>()?;

    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    transaction
        .batch_execute(
            "CREATE TEMP TABLE stage_zone_unit_embeddings ( \
                zone_unit_id text PRIMARY KEY, \
                embedding_fingerprint text NOT NULL, \
                embedding text NOT NULL, \
                model text NOT NULL, \
                dimension integer NOT NULL \
             ) ON COMMIT DROP;",
        )
        .map_err(StorageError::PostgresClient)?;
    transaction
        .execute(
            "INSERT INTO stage_zone_unit_embeddings \
                (zone_unit_id, embedding_fingerprint, embedding, model, dimension) \
             SELECT * FROM unnest($1::text[], $2::text[], $3::text[], $4::text[], $5::int[]);",
            &[&ids, &fingerprints, &literals, &models, &dimensions],
        )
        .map_err(StorageError::PostgresClient)?;

    let updated = transaction
        .execute(
            "UPDATE zone_units u \
             SET embedding_fingerprint = s.embedding_fingerprint \
             FROM stage_zone_unit_embeddings s \
             WHERE u.zone_unit_id = s.zone_unit_id \
               AND (u.embedding_fingerprint IS NULL \
                    OR u.embedding_fingerprint = s.embedding_fingerprint);",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    if updated as usize != embeddings.len() {
        let offender = transaction
            .query_opt(
                "SELECT s.zone_unit_id, s.embedding_fingerprint \
                 FROM stage_zone_unit_embeddings s \
                 LEFT JOIN zone_units u ON u.zone_unit_id = s.zone_unit_id \
                 WHERE u.zone_unit_id IS NULL \
                    OR (u.embedding_fingerprint IS NOT NULL \
                        AND u.embedding_fingerprint <> s.embedding_fingerprint) \
                 ORDER BY s.zone_unit_id \
                 LIMIT 1;",
                &[],
            )
            .map_err(StorageError::PostgresClient)?;
        let message = offender
            .map(|row| {
                let zone_unit_id: String = row.get(0);
                let fingerprint: String = row.get(1);
                format!(
                    "zone unit `{zone_unit_id}` is missing or has a different embedding fingerprint than `{fingerprint}`"
                )
            })
            .unwrap_or_else(|| {
                "a staged zone unit is missing or has a conflicting embedding fingerprint".to_owned()
            });
        return Err(StorageError::Projection { message });
    }

    transaction
        .execute(
            "INSERT INTO zone_unit_embeddings \
                (zone_unit_id, embedding_fingerprint, embedding, model, dimension) \
             SELECT zone_unit_id, embedding_fingerprint, embedding::vector, model, dimension \
             FROM stage_zone_unit_embeddings \
             ON CONFLICT (zone_unit_id) DO UPDATE SET \
                embedding_fingerprint = EXCLUDED.embedding_fingerprint, \
                embedding = EXCLUDED.embedding, \
                model = EXCLUDED.model, \
                dimension = EXCLUDED.dimension;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(embeddings.len())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneDenseRebuildReport {
    pub zone_units: i64,
    pub embeddings: i64,
    pub embedding_fingerprint: String,
    pub index_name: String,
    pub index_lists: u32,
}

/// Zone-unit equivalent of `finalize_dense_rebuild`: verify full embedding coverage, stamp the
/// fingerprint, and (re)build the zone-unit ivfflat index. Touches ONLY the zone tables/index — the
/// chunk dense path is untouched (Option B isolation). Refuses on an empty zone corpus or any unit
/// missing an embedding (the finalize-gap guard).
pub fn finalize_zone_dense_rebuild(
    postgres: &ManagedPostgres,
    spec: &DenseRebuildSpec<'_>,
) -> Result<ZoneDenseRebuildReport, StorageError> {
    validate_zone_dense_spec(spec)?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;

    let zone_units: i64 = transaction
        .query_one("SELECT count(*) FROM zone_units;", &[])
        .map_err(StorageError::PostgresClient)?
        .get(0);
    if zone_units == 0 {
        return Err(StorageError::DenseRebuild {
            message: "cannot finalize zone dense rebuild for an empty zone-unit corpus".to_owned(),
        });
    }
    let embeddings: i64 = transaction
        .query_one(
            "SELECT count(*) FROM zone_unit_embeddings \
             WHERE embedding_fingerprint = $1 AND model = $2 AND dimension = $3;",
            &[&spec.embedding_fingerprint, &spec.model, &spec.dimension],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    let missing: i64 = transaction
        .query_one(
            "SELECT count(*) FROM zone_units u \
             LEFT JOIN zone_unit_embeddings e ON e.zone_unit_id = u.zone_unit_id \
             WHERE e.zone_unit_id IS NULL \
                OR e.embedding_fingerprint <> $1 \
                OR e.model <> $2 \
                OR e.dimension <> $3;",
            &[&spec.embedding_fingerprint, &spec.model, &spec.dimension],
        )
        .map_err(StorageError::PostgresClient)?
        .get(0);
    if missing != 0 {
        return Err(StorageError::DenseRebuild {
            message: format!(
                "{missing} zone units are missing embeddings for fingerprint `{}`",
                spec.embedding_fingerprint
            ),
        });
    }

    transaction
        .execute(
            "UPDATE zone_units SET embedding_fingerprint = $1;",
            &[&spec.embedding_fingerprint],
        )
        .map_err(StorageError::PostgresClient)?;
    transaction
        .batch_execute(&format!(
            "DROP INDEX IF EXISTS {index_name}; \
             CREATE INDEX {index_name} \
             ON zone_unit_embeddings USING ivfflat (embedding vector_l2_ops) \
             WITH (lists = {lists}); \
             ANALYZE zone_units; \
             ANALYZE zone_unit_embeddings;",
            index_name = ZONE_UNIT_VECTOR_INDEX_NAME,
            lists = spec.index_lists
        ))
        .map_err(StorageError::PostgresClient)?;

    let manifest = serde_json::json!({
        "embedding_fingerprint": spec.embedding_fingerprint,
        "model": spec.model,
        "dimension": spec.dimension,
        "normalize": spec.normalize,
        "vector_index": {
            "name": ZONE_UNIT_VECTOR_INDEX_NAME,
            "method": "ivfflat",
            "operator_class": "vector_l2_ops",
            "lists": spec.index_lists
        },
        "coverage": { "zone_units": zone_units, "embeddings": embeddings }
    })
    .to_string();
    transaction
        .execute(
            "INSERT INTO index_manifest(key, value, updated_at) \
             VALUES ('zone_embedding', $1::text::jsonb, now()) \
             ON CONFLICT (key) DO UPDATE \
             SET value = EXCLUDED.value, updated_at = EXCLUDED.updated_at;",
            &[&manifest],
        )
        .map_err(StorageError::PostgresClient)?;
    transaction.commit().map_err(StorageError::PostgresClient)?;

    Ok(ZoneDenseRebuildReport {
        zone_units,
        embeddings,
        embedding_fingerprint: spec.embedding_fingerprint.to_owned(),
        index_name: ZONE_UNIT_VECTOR_INDEX_NAME.to_owned(),
        index_lists: spec.index_lists,
    })
}

fn validate_zone_dense_spec(spec: &DenseRebuildSpec<'_>) -> Result<(), StorageError> {
    if spec.embedding_fingerprint.trim().is_empty() || spec.model.trim().is_empty() {
        return Err(StorageError::DenseRebuild {
            message: "embedding_fingerprint and model must not be empty".to_owned(),
        });
    }
    if spec.dimension != crate::dense::DENSE_VECTOR_DIMENSION {
        return Err(StorageError::DenseRebuild {
            message: format!(
                "zone dense rebuild dimension must match schema vector({}), got {}",
                crate::dense::DENSE_VECTOR_DIMENSION,
                spec.dimension
            ),
        });
    }
    let expected = format!(
        "{}:{}:normalize:{}",
        spec.model, spec.dimension, spec.normalize
    );
    if spec.embedding_fingerprint != expected {
        return Err(StorageError::DenseRebuild {
            message: format!(
                "embedding_fingerprint `{}` does not match model/dimension/normalize spec `{expected}`",
                spec.embedding_fingerprint
            ),
        });
    }
    if spec.index_lists == 0 {
        return Err(StorageError::DenseRebuild {
            message: "index_lists must be at least 1".to_owned(),
        });
    }
    Ok(())
}

/// Coverage report for the zone-retrieval subsystem (the `status.zone_retrieval` block). A SEPARATE
/// surface from the Phase 2 corpus gate — it reports the per-decision official overlay's reach
/// (enrichment attempts by source/status, derived units by zone, embedding coverage), never inflating
/// the corpus claim. Counts run over the small zone/overlay tables only (no 1.1M-row resolver scan).
pub fn zone_retrieval_coverage_json(postgres: &ManagedPostgres) -> Result<String, StorageError> {
    postgres.execute_sql(
        r#"
SELECT jsonb_build_object(
    'scope', 'official_cour_de_cassation_zones (cass+inca)',
    'decision_zones', jsonb_build_object(
        'total', (SELECT count(*) FROM decision_zones),
        'by_source_status', COALESCE((
            SELECT jsonb_agg(jsonb_build_object('source', source, 'status', status, 'count', n)
                             ORDER BY source, status)
            FROM (
                SELECT d.source AS source, z.status AS status, count(*) AS n
                FROM decision_zones z JOIN documents d ON d.document_id = z.document_id
                GROUP BY d.source, z.status
            ) s
        ), '[]'::jsonb)
    ),
    'zone_units', jsonb_build_object(
        'total', (SELECT count(*) FROM zone_units),
        'decisions', (SELECT count(DISTINCT document_id) FROM zone_units),
        'by_zone', COALESCE((
            SELECT jsonb_agg(jsonb_build_object('zone', zone, 'count', n) ORDER BY zone)
            FROM (SELECT zone, count(*) AS n FROM zone_units GROUP BY zone) z
        ), '[]'::jsonb)
    ),
    'embeddings', jsonb_build_object(
        'total', (SELECT count(*) FROM zone_unit_embeddings),
        'units_pending', (
            SELECT count(*) FROM zone_units u
            LEFT JOIN zone_unit_embeddings e ON e.zone_unit_id = u.zone_unit_id
            WHERE e.zone_unit_id IS NULL
        )
    ),
    'embedding_manifest', (SELECT value FROM index_manifest WHERE key = 'zone_embedding')
)::text;
"#,
    )
}

/// Comma-separated SQL string-literal IN-list of the [`ZONE_ENRICHABLE_SOURCES`] (e.g. `'cass','inca'`).
fn zone_enrichable_sources_in_list() -> String {
    ZONE_ENRICHABLE_SOURCES
        .iter()
        .map(|source| sql_string_literal(source))
        .collect::<Vec<_>>()
        .join(",")
}

/// Resolver-reachable DENOMINATOR for the zone overlay (the honest base of "how many Cassation
/// decisions COULD ever carry official zones"). SEPARATE from [`zone_retrieval_coverage_json`] because
/// it is a full scan of the cass/inca decisions (`PARSER_VALID_POURVOI_EXISTS` over ~1.1M rows) — too
/// expensive for the zone-search hot path, acceptable for the operator `status` command. Counts, per
/// source, the decisions that the Judilibre resolver can reach (parser-valid pourvoi) vs. those skipped
/// for lack of one — using the EXACT predicate `enrich_zone_candidates_json` gates on, so the
/// denominator matches what the backfill actually attempts. Returns
/// `{ "by_source": [{source, total, resolver_reachable, skipped_no_pourvoi}], "resolver_reachable_total" }`.
pub fn zone_resolver_reachable_json(postgres: &ManagedPostgres) -> Result<String, StorageError> {
    let source_list = zone_enrichable_sources_in_list();
    postgres.execute_sql(&format!(
        r#"
WITH reach AS (
    SELECT d.source AS source,
           count(*) AS total,
           count(*) FILTER (WHERE {PARSER_VALID_POURVOI_EXISTS}) AS reachable
    FROM documents d
    WHERE d.kind = 'decision'
      AND d.source IN ({source_list})
    GROUP BY d.source
)
SELECT jsonb_build_object(
    'by_source', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'source', source,
            'total', total,
            'resolver_reachable', reachable,
            'skipped_no_pourvoi', total - reachable
        ) ORDER BY source)
        FROM reach
    ), '[]'::jsonb),
    'resolver_reachable_total', COALESCE((SELECT sum(reachable) FROM reach), 0)
)::text;
"#
    ))
}
