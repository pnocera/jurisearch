use jurisearch_ingest::legi::{
    CanonicalDocument, CanonicalGraphEdge, ParsedSectionTa, ParsedTextStruct, ParsedTextVersion,
};

use crate::runtime::{ManagedPostgres, StorageError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalInsertReport {
    pub documents: usize,
    pub chunks: usize,
    pub publisher_edges: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegiMetadataInsertReport {
    pub metadata_roots: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegiHierarchyBackfillReport {
    pub documents_updated: usize,
    pub embeddings_invalidated: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkEmbeddingInsert<'a> {
    pub chunk_id: &'a str,
    pub embedding_fingerprint: &'a str,
    pub embedding_literal: &'a str,
    pub model: &'a str,
    pub dimension: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegiMetadataRoot<'a> {
    TextVersion(&'a ParsedTextVersion),
    SectionTa(&'a ParsedSectionTa),
    TextStruct(&'a ParsedTextStruct),
}

pub fn insert_legi_documents(
    postgres: &ManagedPostgres,
    documents: &[CanonicalDocument],
    chunk_embedding_fingerprint: Option<&str>,
) -> Result<CanonicalInsertReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;

    let document_statement = transaction
        .prepare(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, version_group, citation, title, body, \
                 valid_from, valid_to, valid_to_raw, source_url, source_payload_hash, canonical_json) \
             VALUES \
                ($1, $2, $3, $4, $5, $6, $7, $8, \
                 $9::text::date, $10::text::date, $11, $12, $13, $14::text::jsonb) \
             ON CONFLICT (document_id) DO UPDATE SET \
                source = EXCLUDED.source, \
                kind = EXCLUDED.kind, \
                source_uid = EXCLUDED.source_uid, \
                version_group = EXCLUDED.version_group, \
                citation = EXCLUDED.citation, \
                title = EXCLUDED.title, \
                body = EXCLUDED.body, \
                valid_from = EXCLUDED.valid_from, \
                valid_to = EXCLUDED.valid_to, \
                valid_to_raw = EXCLUDED.valid_to_raw, \
                source_url = EXCLUDED.source_url, \
                source_payload_hash = EXCLUDED.source_payload_hash, \
                canonical_json = EXCLUDED.canonical_json, \
                updated_at = now();",
        )
        .map_err(StorageError::PostgresClient)?;
    let chunk_statement = transaction
        .prepare(
            "INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, chunk_kind, source_fields, \
                 source_payload_hash, chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ($1, $2, $3, $4, $5, $6::text::jsonb, $7, $8, $9) \
             ON CONFLICT (chunk_id) DO UPDATE SET \
                document_id = EXCLUDED.document_id, \
                chunk_index = EXCLUDED.chunk_index, \
                body = EXCLUDED.body, \
                chunk_kind = EXCLUDED.chunk_kind, \
                source_fields = EXCLUDED.source_fields, \
                source_payload_hash = EXCLUDED.source_payload_hash, \
                chunk_builder_version = EXCLUDED.chunk_builder_version, \
                embedding_fingerprint = EXCLUDED.embedding_fingerprint;",
        )
        .map_err(StorageError::PostgresClient)?;
    let edge_statement = transaction
        .prepare(
            "INSERT INTO graph_edges \
                (edge_id, from_document_id, to_document_id, edge_kind, edge_source, payload) \
             VALUES ($1, $2, $3, $4, $5, $6::text::jsonb) \
             ON CONFLICT (edge_id) DO UPDATE SET \
                from_document_id = EXCLUDED.from_document_id, \
                to_document_id = EXCLUDED.to_document_id, \
                edge_kind = EXCLUDED.edge_kind, \
                edge_source = EXCLUDED.edge_source, \
                payload = EXCLUDED.payload;",
        )
        .map_err(StorageError::PostgresClient)?;

    let mut chunks = 0usize;
    let mut publisher_edges = 0usize;
    for document in documents {
        document
            .validate()
            .map_err(|error| StorageError::Projection {
                message: format!(
                    "canonical document `{}` failed validation before storage projection: {error}",
                    document.document_id
                ),
            })?;
        let canonical_json = serde_json::to_string(document)?;
        transaction
            .execute(
                &document_statement,
                &[
                    &document.document_id,
                    &document.source,
                    &document.kind,
                    &document.source_uid,
                    &document.version_group,
                    &document.citation,
                    &document.title,
                    &document.body,
                    &document.valid_from,
                    &document.valid_to,
                    &document.valid_to_raw,
                    &document.source_url,
                    &document.source_payload_hash,
                    &canonical_json,
                ],
            )
            .map_err(StorageError::PostgresClient)?;

        for chunk in &document.chunks {
            let source_fields = serde_json::to_string(&chunk.source_fields)?;
            let chunk_index =
                i32::try_from(chunk.chunk_index).map_err(|_| StorageError::Projection {
                    message: format!(
                        "chunk `{}` has chunk_index too large for storage: {}",
                        chunk.chunk_id, chunk.chunk_index
                    ),
                })?;
            transaction
                .execute(
                    &chunk_statement,
                    &[
                        &chunk.chunk_id,
                        &chunk.document_id,
                        &chunk_index,
                        &chunk.body,
                        &chunk.chunk_kind,
                        &source_fields,
                        &chunk.source_payload_hash,
                        &chunk.chunk_builder_version,
                        &chunk_embedding_fingerprint,
                    ],
                )
                .map_err(StorageError::PostgresClient)?;
            chunks += 1;
        }

        for edge in &document.publisher_edges {
            insert_publisher_edge(&mut transaction, &edge_statement, edge)?;
            publisher_edges += 1;
        }
    }

    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(CanonicalInsertReport {
        documents: documents.len(),
        chunks,
        publisher_edges,
    })
}

pub fn insert_legi_metadata_roots(
    postgres: &ManagedPostgres,
    roots: &[LegiMetadataRoot<'_>],
) -> Result<LegiMetadataInsertReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    let statement = transaction
        .prepare(
            "INSERT INTO legi_metadata_roots \
                (metadata_key, root_kind, source_uid, parent_source_uid, title, \
                 valid_from, valid_to, valid_to_raw, source_payload_hash, source_archive, \
                 source_member_path, canonical_version, canonical_json) \
             VALUES \
                ($1, $2, $3, $4, $5, \
                 $6::text::date, $7::text::date, $8, $9, $10, \
                 $11, $12, $13::text::jsonb) \
             ON CONFLICT (metadata_key) DO UPDATE SET \
                root_kind = EXCLUDED.root_kind, \
                source_uid = EXCLUDED.source_uid, \
                parent_source_uid = EXCLUDED.parent_source_uid, \
                title = EXCLUDED.title, \
                valid_from = EXCLUDED.valid_from, \
                valid_to = EXCLUDED.valid_to, \
                valid_to_raw = EXCLUDED.valid_to_raw, \
                source_payload_hash = EXCLUDED.source_payload_hash, \
                source_archive = EXCLUDED.source_archive, \
                source_member_path = EXCLUDED.source_member_path, \
                canonical_version = EXCLUDED.canonical_version, \
                canonical_json = EXCLUDED.canonical_json, \
                updated_at = now();",
        )
        .map_err(StorageError::PostgresClient)?;

    for root in roots {
        let row = LegiMetadataRow::from_root(*root)?;
        transaction
            .execute(
                &statement,
                &[
                    &row.metadata_key,
                    &row.root_kind,
                    &row.source_uid,
                    &row.parent_source_uid,
                    &row.title,
                    &row.valid_from,
                    &row.valid_to,
                    &row.valid_to_raw,
                    &row.source_payload_hash,
                    &row.source_archive,
                    &row.source_member_path,
                    &row.canonical_version,
                    &row.canonical_json,
                ],
            )
            .map_err(StorageError::PostgresClient)?;
    }

    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(LegiMetadataInsertReport {
        metadata_roots: roots.len(),
    })
}

pub fn backfill_legi_article_hierarchy_from_metadata(
    postgres: &ManagedPostgres,
) -> Result<LegiHierarchyBackfillReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let rows = client
        .query(
            "SELECT DISTINCT ON (d.document_id) \
                d.document_id, d.canonical_json::text, section.canonical_json::text \
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
             ORDER BY d.document_id, section.valid_from DESC NULLS LAST, section.metadata_key;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;

    let mut updates = Vec::<(String, String)>::new();
    for row in rows {
        let document_id: String = row.get(0);
        let document_json: String = row.get(1);
        let section_json: String = row.get(2);
        if let Some(enriched) = enriched_article_hierarchy_json(&document_json, &section_json)? {
            updates.push((document_id, enriched));
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
                 updated_at = now() \
             WHERE document_id = $1;",
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
    }

    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(LegiHierarchyBackfillReport {
        documents_updated: updates.len(),
        embeddings_invalidated,
    })
}

pub fn insert_chunk_embeddings(
    postgres: &ManagedPostgres,
    embeddings: &[ChunkEmbeddingInsert<'_>],
) -> Result<usize, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    let statement = transaction
        .prepare(
            "INSERT INTO chunk_embeddings \
                (chunk_id, embedding_fingerprint, embedding, model, dimension) \
             VALUES ($1, $2, $3::text::vector, $4, $5) \
             ON CONFLICT (chunk_id) DO UPDATE SET \
                embedding_fingerprint = EXCLUDED.embedding_fingerprint, \
                embedding = EXCLUDED.embedding, \
                model = EXCLUDED.model, \
                dimension = EXCLUDED.dimension;",
        )
        .map_err(StorageError::PostgresClient)?;
    let chunk_fingerprint_statement = transaction
        .prepare(
            "UPDATE chunks \
             SET embedding_fingerprint = $2 \
             WHERE chunk_id = $1 \
               AND (embedding_fingerprint IS NULL OR embedding_fingerprint = $2);",
        )
        .map_err(StorageError::PostgresClient)?;

    for embedding in embeddings {
        let dimension =
            i32::try_from(embedding.dimension).map_err(|_| StorageError::Projection {
                message: format!(
                    "embedding dimension too large for storage on chunk `{}`: {}",
                    embedding.chunk_id, embedding.dimension
                ),
            })?;
        let updated = transaction
            .execute(
                &chunk_fingerprint_statement,
                &[&embedding.chunk_id, &embedding.embedding_fingerprint],
            )
            .map_err(StorageError::PostgresClient)?;
        if updated != 1 {
            return Err(StorageError::Projection {
                message: format!(
                    "chunk `{}` is missing or has a different embedding fingerprint than `{}`",
                    embedding.chunk_id, embedding.embedding_fingerprint
                ),
            });
        }
        transaction
            .execute(
                &statement,
                &[
                    &embedding.chunk_id,
                    &embedding.embedding_fingerprint,
                    &embedding.embedding_literal,
                    &embedding.model,
                    &dimension,
                ],
            )
            .map_err(StorageError::PostgresClient)?;
    }

    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(embeddings.len())
}

fn enriched_article_hierarchy_json(
    document_json: &str,
    section_json: &str,
) -> Result<Option<String>, StorageError> {
    let mut document: serde_json::Value = serde_json::from_str(document_json)?;
    let section: serde_json::Value = serde_json::from_str(section_json)?;
    let mut hierarchy = string_array_field(&section, "hierarchy_path");
    if let Some(section_title) = section.get("title").and_then(serde_json::Value::as_str)
        && hierarchy.last().is_none_or(|last| last != section_title)
    {
        hierarchy.push(section_title.to_owned());
    }
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

fn string_array_field(value: &serde_json::Value, field: &str) -> Vec<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect()
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

struct LegiMetadataRow {
    metadata_key: String,
    root_kind: &'static str,
    source_uid: Option<String>,
    parent_source_uid: Option<String>,
    title: Option<String>,
    valid_from: Option<String>,
    valid_to: Option<String>,
    valid_to_raw: Option<String>,
    source_payload_hash: String,
    source_archive: Option<String>,
    source_member_path: Option<String>,
    canonical_version: String,
    canonical_json: String,
}

impl LegiMetadataRow {
    fn from_root(root: LegiMetadataRoot<'_>) -> Result<Self, StorageError> {
        match root {
            LegiMetadataRoot::TextVersion(text) => Ok(Self {
                metadata_key: legi_metadata_key(
                    "TEXTE_VERSION",
                    Some(text.text_id.as_str()),
                    Some(text.valid_from.as_str()),
                    text.source_payload_hash.as_str(),
                ),
                root_kind: "TEXTE_VERSION",
                source_uid: Some(text.text_id.clone()),
                parent_source_uid: None,
                title: Some(text.title.clone()),
                valid_from: Some(text.valid_from.clone()),
                valid_to: text.valid_to.clone(),
                valid_to_raw: text.valid_to_raw.clone(),
                source_payload_hash: text.source_payload_hash.clone(),
                source_archive: text.source_archive.clone(),
                source_member_path: text.source_member_path.clone(),
                canonical_version: text.canonical_version.clone(),
                canonical_json: serde_json::to_string(text)?,
            }),
            LegiMetadataRoot::SectionTa(section) => {
                let source_uid = section.section_id.clone();
                Ok(Self {
                    metadata_key: legi_metadata_key(
                        "SECTION_TA",
                        source_uid.as_deref(),
                        Some(section.valid_from.as_str()),
                        section.source_payload_hash.as_str(),
                    ),
                    root_kind: "SECTION_TA",
                    source_uid,
                    parent_source_uid: section.parent_text_id.clone(),
                    title: Some(section.title.clone()),
                    valid_from: Some(section.valid_from.clone()),
                    valid_to: section.valid_to.clone(),
                    valid_to_raw: section.valid_to_raw.clone(),
                    source_payload_hash: section.source_payload_hash.clone(),
                    source_archive: section.source_archive.clone(),
                    source_member_path: section.source_member_path.clone(),
                    canonical_version: section.canonical_version.clone(),
                    canonical_json: serde_json::to_string(section)?,
                })
            }
            LegiMetadataRoot::TextStruct(text_struct) => Ok(Self {
                metadata_key: legi_text_struct_metadata_key(text_struct),
                root_kind: "TEXTELR",
                source_uid: Some(text_struct.text_id.clone()),
                parent_source_uid: None,
                title: None,
                valid_from: text_struct.source_date_debut_hint.clone(),
                valid_to: None,
                valid_to_raw: None,
                source_payload_hash: text_struct.source_payload_hash.clone(),
                source_archive: text_struct.source_archive.clone(),
                source_member_path: text_struct.source_member_path.clone(),
                canonical_version: text_struct.canonical_version.clone(),
                canonical_json: serde_json::to_string(text_struct)?,
            }),
        }
    }
}

fn legi_text_struct_metadata_key(text_struct: &ParsedTextStruct) -> String {
    let digest = source_payload_digest(text_struct.source_payload_hash.as_str());
    match text_struct.source_date_debut_hint.as_deref() {
        Some(date_anchor) => format!(
            "legi:TEXTELR:{}@{date_anchor}:{digest}",
            text_struct.text_id
        ),
        None => format!("legi:TEXTELR:{}:{digest}", text_struct.text_id),
    }
}

fn legi_metadata_key(
    root_kind: &str,
    source_uid: Option<&str>,
    date_anchor: Option<&str>,
    source_payload_hash: &str,
) -> String {
    let fallback = source_payload_digest(source_payload_hash);
    match (source_uid, date_anchor) {
        (Some(uid), Some(date_anchor)) => format!("legi:{root_kind}:{uid}@{date_anchor}"),
        (Some(uid), None) => format!("legi:{root_kind}:{uid}"),
        (None, Some(date_anchor)) => format!("legi:{root_kind}:payload:{fallback}@{date_anchor}"),
        (None, None) => format!("legi:{root_kind}:payload:{fallback}"),
    }
}

fn source_payload_digest(source_payload_hash: &str) -> &str {
    source_payload_hash
        .strip_prefix("sha256:")
        .unwrap_or(source_payload_hash)
}

fn insert_publisher_edge(
    transaction: &mut postgres::Transaction<'_>,
    statement: &postgres::Statement,
    edge: &CanonicalGraphEdge,
) -> Result<(), StorageError> {
    let payload = serde_json::to_string(edge)?;
    transaction
        .execute(
            statement,
            &[
                &edge.edge_id,
                &edge.from_document_id,
                &edge.to_document_id,
                &edge.relation,
                &edge.edge_source,
                &payload,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}
