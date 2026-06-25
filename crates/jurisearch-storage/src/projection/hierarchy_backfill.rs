//! LEGI article-hierarchy backfill from metadata roots (candidate selection, anchor, overlap merge).

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegiHierarchyBackfillReport {
    pub documents_updated: usize,
    pub embeddings_invalidated: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LegiHierarchyBackfillScope {
    pub document_ids: Vec<String>,
    pub section_source_uids: Vec<String>,
    pub text_source_uids: Vec<String>,
}

impl LegiHierarchyBackfillScope {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.document_ids.is_empty()
            && self.section_source_uids.is_empty()
            && self.text_source_uids.is_empty()
    }
}

pub fn backfill_legi_article_hierarchy_from_metadata(
    postgres: &ManagedPostgres,
) -> Result<LegiHierarchyBackfillReport, StorageError> {
    backfill_legi_article_hierarchy_from_metadata_scoped(
        postgres,
        &LegiHierarchyBackfillScope::default(),
    )
}

/// Backfill LEGI article hierarchy for the provided scope.
///
/// An empty scope is intentionally interpreted as a full maintenance backfill.
pub fn backfill_legi_article_hierarchy_from_metadata_scoped(
    postgres: &ManagedPostgres,
    scope: &LegiHierarchyBackfillScope,
) -> Result<LegiHierarchyBackfillReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let full_scope = scope.is_empty();
    let rows = client
        .query(
            // Prefer direct article publisher section edges when present. TEXTELR
            // supplies a fallback candidate by pairing each LIEN_ART with the
            // nearest preceding LIEN_SECTION_TA in the same flat STRUCT sequence.
            // Candidate selection below then chooses the section version whose
            // validity contains the link/document anchor, falling back to the
            // latest section row when source dates are incomplete. TEXTELR
            // anchors intentionally reuse the preserved raw attributes[] `debut`.
            "WITH hierarchy_candidates AS ( \
                SELECT d.document_id, d.canonical_json::text AS document_json, \
                       d.valid_from::text AS document_valid_from, \
                       edge.payload::text AS edge_payload, \
                       section.canonical_json::text AS section_json, \
                       section.valid_from::text AS section_valid_from, \
                       section.valid_to::text AS section_valid_to, \
                       NULL::text AS text_section_links_json, \
                       0 AS source_rank, section.metadata_key AS section_metadata_key, \
                       edge.edge_id AS tie_breaker \
                FROM documents d \
                JOIN graph_edges edge \
                  ON edge.from_document_id = d.document_id \
                 AND edge.edge_source = 'publisher' \
                 AND edge.payload->>'source_tag' = 'LIEN_SECTION_TA' \
                JOIN legi_metadata_roots section \
                  ON section.root_kind = 'SECTION_TA' \
                 AND section.source_uid = edge.payload->>'to_source_uid' \
                WHERE d.source = 'legi' \
                  AND d.kind = 'article' \
                  AND ( \
                       $1::boolean \
                       OR d.document_id = ANY($2::text[]) \
                       OR edge.payload->>'to_source_uid' = ANY($3::text[]) \
                  ) \
                UNION ALL \
                SELECT d.document_id, d.canonical_json::text AS document_json, \
                       d.valid_from::text AS document_valid_from, \
                       article_link.link::text AS edge_payload, \
                       section.canonical_json::text AS section_json, \
                       section.valid_from::text AS section_valid_from, \
                       section.valid_to::text AS section_valid_to, \
                       text_sections.links::text AS text_section_links_json, \
                       1 AS source_rank, section.metadata_key AS section_metadata_key, \
                       /* Branch-local deterministic tiebreaker; source_rank keeps it separate \
                          from graph edge ids in the direct branch. */ \
                       text_struct.metadata_key || ':' || article_link.ordinality::text AS tie_breaker \
                FROM documents d \
                JOIN legi_metadata_roots text_struct \
                  ON text_struct.root_kind = 'TEXTELR' \
                JOIN LATERAL jsonb_array_elements( \
                       coalesce(text_struct.canonical_json->'structure_links', '[]'::jsonb) \
                     ) WITH ORDINALITY AS article_link(link, ordinality) \
                  ON article_link.link->>'source_tag' = 'LIEN_ART' \
                 AND article_link.link->>'target_source_uid' = d.source_uid \
                JOIN LATERAL ( \
                    SELECT section_link.link \
                    FROM jsonb_array_elements( \
                           coalesce(text_struct.canonical_json->'structure_links', '[]'::jsonb) \
                         ) WITH ORDINALITY AS section_link(link, ordinality) \
                    WHERE section_link.ordinality < article_link.ordinality \
                      AND section_link.link->>'source_tag' = 'LIEN_SECTION_TA' \
                      AND section_link.link->>'target_source_uid' IS NOT NULL \
                    ORDER BY section_link.ordinality DESC \
                    LIMIT 1 \
                ) text_section ON true \
                JOIN LATERAL ( \
                    SELECT jsonb_agg(section_link.link ORDER BY section_link.ordinality) AS links \
                    FROM jsonb_array_elements( \
                           coalesce(text_struct.canonical_json->'structure_links', '[]'::jsonb) \
                         ) WITH ORDINALITY AS section_link(link, ordinality) \
                    WHERE section_link.ordinality < article_link.ordinality \
                      AND section_link.link->>'source_tag' = 'LIEN_SECTION_TA' \
                ) text_sections ON true \
                JOIN legi_metadata_roots section \
                  ON section.root_kind = 'SECTION_TA' \
                 AND section.source_uid = text_section.link->>'target_source_uid' \
                WHERE d.source = 'legi' \
                  AND d.kind = 'article' \
                  /* Direct publisher section edges are treated as authoritative; \
                     TEXTELR fallback only fills articles that lack one. */ \
                  AND NOT EXISTS ( \
                      SELECT 1 \
                      FROM graph_edges direct_edge \
                      WHERE direct_edge.from_document_id = d.document_id \
                        AND direct_edge.edge_source = 'publisher' \
                        AND direct_edge.payload->>'source_tag' = 'LIEN_SECTION_TA' \
                  ) \
                  AND ( \
                       $1::boolean \
                       OR d.document_id = ANY($2::text[]) \
                       OR text_section.link->>'target_source_uid' = ANY($3::text[]) \
                       OR text_struct.source_uid = ANY($4::text[]) \
                  ) \
            ) \
             SELECT document_id, document_json, document_valid_from, edge_payload, \
                    section_json, section_valid_from, section_valid_to, text_section_links_json \
             FROM hierarchy_candidates \
             ORDER BY document_id, source_rank, section_valid_from DESC NULLS LAST, \
                      section_metadata_key, tie_breaker;",
            &[
                &full_scope,
                &scope.document_ids,
                &scope.section_source_uids,
                &scope.text_source_uids,
            ],
        )
        .map_err(StorageError::PostgresClient)?;

    let mut updates = Vec::<(String, String)>::new();
    let mut candidates = Vec::with_capacity(rows.len());
    for row in rows {
        candidates.push(HierarchyBackfillCandidate {
            document_id: row.get(0),
            document_json: row.get(1),
            document_valid_from: row.get(2),
            edge_payload: row.get(3),
            section_json: row.get(4),
            section_valid_from: row.get(5),
            section_valid_to: row.get(6),
            text_section_links_json: row.get(7),
        });
    }

    for candidate in select_hierarchy_backfill_candidates(candidates)? {
        if let Some(enriched) = enriched_article_hierarchy_json(
            &candidate.document_json,
            &candidate.section_json,
            candidate.text_section_links_json.as_deref(),
        )? {
            updates.push((candidate.document_id, enriched));
        }
    }

    if updates.is_empty() {
        return Ok(LegiHierarchyBackfillReport {
            documents_updated: 0,
            embeddings_invalidated: 0,
        });
    }

    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    let update_document = transaction
        .prepare(
            "UPDATE documents \
             SET canonical_json = $2::text::jsonb, \
                 hierarchy_path = COALESCE($2::text::jsonb->'hierarchy_path', hierarchy_path), \
                 updated_at = now() \
             WHERE document_id = $1;",
        )
        .map_err(StorageError::PostgresClient)?;
    let update_chunks = transaction
        .prepare(
            "UPDATE chunks c \
             SET contextualized_body = chunk_payload.chunk->>'contextualized_body', \
                 chunking = COALESCE(NULLIF(chunk_payload.chunk->>'chunking', ''), c.chunking), \
                 boundary = COALESCE(NULLIF(chunk_payload.chunk->>'boundary', ''), c.boundary), \
                 hierarchy_path = COALESCE(chunk_payload.chunk->'hierarchy_path', c.hierarchy_path) \
             FROM jsonb_array_elements(coalesce($2::text::jsonb->'chunks', '[]'::jsonb)) \
                  WITH ORDINALITY AS chunk_payload(chunk, ordinality) \
             WHERE c.document_id = $1 \
               AND c.chunk_index = (chunk_payload.ordinality - 1)::integer;",
        )
        .map_err(StorageError::PostgresClient)?;
    let clear_chunk_fingerprints = transaction
        .prepare(
            "UPDATE chunks \
             SET embedding_fingerprint = NULL \
             WHERE document_id = $1;",
        )
        .map_err(StorageError::PostgresClient)?;
    let delete_embeddings = transaction
        .prepare(
            "DELETE FROM chunk_embeddings embedding \
             USING chunks chunk \
             WHERE embedding.chunk_id = chunk.chunk_id \
               AND chunk.document_id = $1;",
        )
        .map_err(StorageError::PostgresClient)?;

    let mut embeddings_invalidated = 0usize;
    for (document_id, canonical_json) in &updates {
        let deleted = transaction
            .execute(&delete_embeddings, &[document_id])
            .map_err(StorageError::PostgresClient)?;
        embeddings_invalidated +=
            usize::try_from(deleted).map_err(|_| StorageError::Projection {
                message: format!(
                    "embedding invalidation count too large for document `{document_id}`: {deleted}"
                ),
            })?;
        transaction
            .execute(&clear_chunk_fingerprints, &[document_id])
            .map_err(StorageError::PostgresClient)?;
        transaction
            .execute(&update_document, &[document_id, canonical_json])
            .map_err(StorageError::PostgresClient)?;
        transaction
            .execute(&update_chunks, &[document_id, canonical_json])
            .map_err(StorageError::PostgresClient)?;
    }

    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(LegiHierarchyBackfillReport {
        documents_updated: updates.len(),
        embeddings_invalidated,
    })
}

#[derive(Debug)]
struct HierarchyBackfillCandidate {
    document_id: String,
    document_json: String,
    document_valid_from: Option<String>,
    edge_payload: String,
    section_json: String,
    section_valid_from: Option<String>,
    section_valid_to: Option<String>,
    text_section_links_json: Option<String>,
}

fn select_hierarchy_backfill_candidates(
    candidates: Vec<HierarchyBackfillCandidate>,
) -> Result<Vec<HierarchyBackfillCandidate>, StorageError> {
    let mut selected = Vec::new();
    let mut current = Vec::new();

    for candidate in candidates {
        if current
            .first()
            .is_some_and(|first: &HierarchyBackfillCandidate| {
                first.document_id != candidate.document_id
            })
        {
            selected.push(select_hierarchy_backfill_candidate(current)?);
            current = Vec::new();
        }
        current.push(candidate);
    }

    if !current.is_empty() {
        selected.push(select_hierarchy_backfill_candidate(current)?);
    }

    Ok(selected)
}

fn select_hierarchy_backfill_candidate(
    mut candidates: Vec<HierarchyBackfillCandidate>,
) -> Result<HierarchyBackfillCandidate, StorageError> {
    for index in 0..candidates.len() {
        let Some(anchor) = hierarchy_backfill_anchor(
            candidates[index].edge_payload.as_str(),
            candidates[index].document_valid_from.as_deref(),
        )?
        else {
            continue;
        };
        if section_validity_contains(
            anchor.as_str(),
            candidates[index].section_valid_from.as_deref(),
            candidates[index].section_valid_to.as_deref(),
        ) {
            return Ok(candidates.remove(index));
        }
    }

    Ok(candidates.remove(0))
}

fn hierarchy_backfill_anchor(
    edge_payload: &str,
    document_valid_from: Option<&str>,
) -> Result<Option<String>, StorageError> {
    let payload: serde_json::Value = serde_json::from_str(edge_payload)?;
    let edge_debut = payload
        .get("attributes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|attribute| {
            let key = attribute.get("key").and_then(serde_json::Value::as_str)?;
            let value = attribute.get("value").and_then(serde_json::Value::as_str)?;
            (key == "debut" && is_iso_date(value)).then(|| value.to_owned())
        });

    Ok(edge_debut.or_else(|| {
        document_valid_from
            .filter(|date| is_iso_date(date))
            .map(str::to_owned)
    }))
}

fn section_validity_contains(
    anchor: &str,
    valid_from: Option<&str>,
    valid_to: Option<&str>,
) -> bool {
    if !is_iso_date(anchor) {
        return false;
    }

    valid_from.is_none_or(|date| is_iso_date(date) && date <= anchor)
        && valid_to.is_none_or(|date| is_iso_date(date) && anchor < date)
}

fn is_iso_date(value: &str) -> bool {
    // Storage comparisons only need the canonical shape; ingest performs
    // semantic date validation before records reach these tables.
    value.as_bytes().iter().enumerate().all(|(index, byte)| {
        if matches!(index, 4 | 7) {
            *byte == b'-'
        } else {
            byte.is_ascii_digit()
        }
    }) && value.len() == 10
}

pub(super) fn enriched_article_hierarchy_json(
    document_json: &str,
    section_json: &str,
    text_section_links_json: Option<&str>,
) -> Result<Option<String>, StorageError> {
    let mut document: serde_json::Value = serde_json::from_str(document_json)?;
    let section: serde_json::Value = serde_json::from_str(section_json)?;
    let section_hierarchy = section_hierarchy_from_json(&section);
    // When TEXTELR adds depth, its ordered TOC labels become the structural
    // branch; at equal or shallower depth, SECTION_TA metadata labels win.
    let hierarchy = match text_section_links_json {
        Some(links_json) => match text_struct_hierarchy_from_links(&section, links_json)? {
            Some(text_hierarchy) if text_hierarchy.len() > section_hierarchy.len() => {
                text_hierarchy
            }
            _ => section_hierarchy,
        },
        None => section_hierarchy,
    };
    let current_hierarchy = string_array_field(&document, "hierarchy_path");
    if hierarchy.is_empty()
        || hierarchy == current_hierarchy
        || hierarchy.len() <= current_hierarchy.len()
    {
        return Ok(None);
    }

    let title = document
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let hierarchy_json = serde_json::json!(hierarchy);
    document["hierarchy_path"] = hierarchy_json.clone();

    if let Some(chunks) = document
        .get_mut("chunks")
        .and_then(serde_json::Value::as_array_mut)
    {
        for chunk in chunks {
            chunk["hierarchy_path"] = hierarchy_json.clone();
            if let Some(body) = chunk.get("body").and_then(serde_json::Value::as_str) {
                chunk["contextualized_body"] = serde_json::json!(contextualized_article_body(
                    &hierarchy,
                    title.as_deref(),
                    body
                ));
            }
        }
    }

    Ok(Some(serde_json::to_string(&document)?))
}

fn section_hierarchy_from_json(section: &serde_json::Value) -> Vec<String> {
    let mut hierarchy = string_array_field(section, "hierarchy_path");
    if let Some(section_title) = section.get("title").and_then(serde_json::Value::as_str)
        && hierarchy.last().is_none_or(|last| last != section_title)
    {
        hierarchy.push(section_title.to_owned());
    }
    hierarchy
}

pub(super) fn text_struct_hierarchy_from_links(
    section: &serde_json::Value,
    links_json: &str,
) -> Result<Option<Vec<String>>, StorageError> {
    let links: Vec<serde_json::Value> = serde_json::from_str(links_json)?;
    let mut stack = Vec::<String>::new();
    for link in links {
        let Some(title) = non_empty_json_str(&link, "text") else {
            continue;
        };
        if let Some(level) = link
            .get("level")
            .and_then(serde_json::Value::as_u64)
            .and_then(|level| usize::try_from(level).ok())
            .filter(|level| *level > 0)
        {
            stack.truncate(level.saturating_sub(1).min(stack.len()));
        }
        if stack.last().is_none_or(|last| last != &title) {
            stack.push(title);
        }
    }
    if stack.is_empty() {
        return Ok(None);
    }

    let mut base = section_hierarchy_from_json(section);
    if let Some(section_title) = section.get("title").and_then(serde_json::Value::as_str)
        && base.last().is_some_and(|last| last == section_title)
    {
        base.pop();
    }
    Ok(Some(merge_hierarchy_with_overlap(base, stack)))
}

pub(super) fn merge_hierarchy_with_overlap(
    mut base: Vec<String>,
    suffix: Vec<String>,
) -> Vec<String> {
    let max_overlap = base.len().min(suffix.len());
    let overlap = (0..=max_overlap)
        .rev()
        .find(|overlap| base[base.len() - overlap..] == suffix[..*overlap])
        .unwrap_or(0);
    base.extend(suffix.into_iter().skip(overlap));
    base
}

fn contextualized_article_body(hierarchy: &[String], title: Option<&str>, body: &str) -> String {
    let mut parts = hierarchy.to_vec();
    if let Some(title) = title {
        parts.push(title.to_owned());
    }
    let context = parts.join(" > ");
    if context.is_empty() {
        body.to_owned()
    } else {
        format!("{context}\n\n{body}")
    }
}
