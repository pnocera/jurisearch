//! LEGI metadata-root projection + metadata-key derivation helpers.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegiMetadataInsertReport {
    pub metadata_roots: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegiMetadataRoot<'a> {
    TextVersion(&'a ParsedTextVersion),
    SectionTa(&'a ParsedSectionTa),
    TextStruct(&'a ParsedTextStruct),
}

pub fn insert_legi_metadata_roots(
    postgres: &ManagedPostgres,
    roots: &[LegiMetadataRoot<'_>],
) -> Result<LegiMetadataInsertReport, StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    let report = insert_legi_metadata_roots_with_client(&mut transaction, roots)?;
    transaction.commit().map_err(StorageError::PostgresClient)?;
    Ok(report)
}

pub fn insert_legi_metadata_roots_with_client<C: GenericClient>(
    client: &mut C,
    roots: &[LegiMetadataRoot<'_>],
) -> Result<LegiMetadataInsertReport, StorageError> {
    let statement = client
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
        client
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

    Ok(LegiMetadataInsertReport {
        metadata_roots: roots.len(),
    })
}

pub(crate) fn string_array_field(value: &serde_json::Value, field: &str) -> Vec<String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect()
}

pub(crate) fn non_empty_json_str(value: &serde_json::Value, field: &str) -> Option<String> {
    let value = value.get(field)?.as_str()?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

pub(crate) struct LegiMetadataRow {
    pub(crate) metadata_key: String,
    pub(crate) root_kind: &'static str,
    pub(crate) source_uid: Option<String>,
    pub(crate) parent_source_uid: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) valid_from: Option<String>,
    pub(crate) valid_to: Option<String>,
    pub(crate) valid_to_raw: Option<String>,
    pub(crate) source_payload_hash: String,
    pub(crate) source_archive: Option<String>,
    pub(crate) source_member_path: Option<String>,
    pub(crate) canonical_version: String,
    pub(crate) canonical_json: String,
}

impl LegiMetadataRow {
    pub(crate) fn from_root(root: LegiMetadataRoot<'_>) -> Result<Self, StorageError> {
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

pub(crate) fn legi_text_struct_metadata_key(text_struct: &ParsedTextStruct) -> String {
    let digest = source_payload_digest(text_struct.source_payload_hash.as_str());
    match text_struct.source_date_debut_hint.as_deref() {
        Some(date_anchor) => format!(
            "legi:TEXTELR:{}@{date_anchor}:{digest}",
            text_struct.text_id
        ),
        None => format!("legi:TEXTELR:{}:{digest}", text_struct.text_id),
    }
}

pub(crate) fn legi_metadata_key(
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

pub(crate) fn source_payload_digest(source_payload_hash: &str) -> &str {
    source_payload_hash
        .strip_prefix("sha256:")
        .unwrap_or(source_payload_hash)
}
