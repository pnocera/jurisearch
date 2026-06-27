//! The writable-app reference model + validator (plan P8, design §8.2/§8.3).
//!
//! `jurisearch_app.app_reference` rows point at server data via SOFT, validated references — never a
//! hard cross-schema FK. [`validate_references`] re-resolves every reference for a corpus against that
//! corpus's ACTIVE generation (read in the SAME transaction it stamps), AFTER the cursor has advanced
//! (the one genuinely-background post-apply step, §7.1). It writes ONLY the `resolved_*` /
//! `validation_status` columns — never the semantic identity columns, and never any server table.
//!
//! Resolution convention (§8.3): pin by `document_id` for "this exact version I saw" (survives
//! incrementals + re-baselines because supersession retains old version rows); `source_uid`/
//! `version_group` + `as_of_date` for "the article applicable at date D", using a HALF-OPEN validity
//! window (`valid_from <= as_of < valid_to`); chunk/zone references are anchored at the document level
//! (derived `chunk_id`/`zone_unit_id` are never pinned).

use postgres::GenericClient;
use postgres::error::SqlState;

use crate::generations::schema_for_generation;
use crate::runtime::{ManagedPostgres, StorageError, sql_identifier};

/// The reference resolution outcome recorded on an `app_reference` row (§8.2 `validation_status`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationStatus {
    /// Not yet validated (the column default).
    Unvalidated,
    /// The target exists; for a pin this is terminal, for a logical ref it matched (and is unchanged).
    Resolved,
    /// A LOGICAL target previously resolved to a different `document_id` — a new version landed.
    Changed,
    /// The reference is well-formed but no target exists in the active generation.
    Missing,
    /// The reference row cannot be interpreted (unsupported kind / insufficient identity columns).
    Invalid,
}

impl ValidationStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ValidationStatus::Unvalidated => "unvalidated",
            ValidationStatus::Resolved => "resolved",
            ValidationStatus::Changed => "changed",
            ValidationStatus::Missing => "missing",
            ValidationStatus::Invalid => "invalid",
        }
    }
}

/// One `app_reference` row's identity (the inputs the resolver reads; the semantic columns are never
/// rewritten by the validator).
#[derive(Debug, Clone)]
struct AppReference {
    reference_id: i64,
    target_kind: String,
    corpus: String,
    document_id: Option<String>,
    source: Option<String>,
    source_uid: Option<String>,
    version_group: Option<String>,
    as_of_date: Option<String>,
    prior_resolved_document_id: Option<String>,
}

/// Counts by status for one validation pass (observability; no app-UX policy).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    /// Whether the corpus was installed (had a `corpus_state` cursor) at validation time.
    pub installed: bool,
    /// The generation + schema the references were validated against (empty when not installed).
    pub generation: String,
    pub schema_version: i32,
    pub resolved: u32,
    pub changed: u32,
    pub missing: u32,
    pub invalid: u32,
}

impl ValidationReport {
    fn count(&mut self, status: ValidationStatus) {
        match status {
            ValidationStatus::Resolved => self.resolved += 1,
            ValidationStatus::Changed => self.changed += 1,
            ValidationStatus::Missing => self.missing += 1,
            ValidationStatus::Invalid => self.invalid += 1,
            ValidationStatus::Unvalidated => {}
        }
    }
}

/// Re-resolve and re-stamp every `app_reference` row for `corpus` against its ACTIVE generation. The
/// cursor read, every document resolution, and the `app_reference` updates run in ONE transaction so the
/// stamped `resolved_generation`/`resolved_schema_version` exactly match the generation that was read.
/// Retries ONCE if the active generation's schema vanished mid-pass (a concurrent re-baseline retiring
/// it). Writes ONLY `resolved_*` / `validated_at` / `validation_status` — never the server tables.
///
/// # Errors
/// [`StorageError`] on a DB error.
pub fn validate_references(
    postgres: &ManagedPostgres,
    corpus: &str,
) -> Result<ValidationReport, StorageError> {
    match validate_once(postgres, corpus) {
        Err(StorageError::PostgresClient(error)) if is_transient_schema_error(&error) => {
            validate_once(postgres, corpus)
        }
        other => other,
    }
}

fn is_transient_schema_error(error: &postgres::Error) -> bool {
    matches!(
        error.code(),
        Some(code) if *code == SqlState::UNDEFINED_TABLE || *code == SqlState::UNDEFINED_SCHEMA
    )
}

fn validate_once(
    postgres: &ManagedPostgres,
    corpus: &str,
) -> Result<ValidationReport, StorageError> {
    let mut client = postgres.client()?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;

    // The corpus cursor + the resolution reads share this transaction snapshot, so the stamped
    // generation/schema are exactly the ones the documents were resolved against.
    let cursor = tx
        .query_opt(
            "SELECT active_generation, schema_version \
             FROM jurisearch_control.corpus_state WHERE corpus = $1;",
            &[&corpus],
        )
        .map_err(StorageError::PostgresClient)?;

    let references = read_app_references(&mut tx, corpus)?;
    let mut report = ValidationReport::default();

    let Some(cursor_row) = cursor else {
        // Corpus not installed → every reference is `missing` (its targets do not exist on this
        // client); never silently fall back to `public`/the views.
        for reference in &references {
            update_reference(
                &mut tx,
                reference.reference_id,
                None,
                None,
                None,
                ValidationStatus::Missing,
            )?;
            report.count(ValidationStatus::Missing);
        }
        tx.commit().map_err(StorageError::PostgresClient)?;
        report.installed = false;
        return Ok(report);
    };

    let active_generation: String = cursor_row.get("active_generation");
    let schema_version: i32 = cursor_row.get("schema_version");
    let schema = schema_for_generation(&active_generation);

    for reference in &references {
        let (resolved, status) = resolve_reference(&mut tx, &schema, reference)?;
        update_reference(
            &mut tx,
            reference.reference_id,
            resolved.as_deref(),
            Some(active_generation.as_str()),
            Some(schema_version),
            status,
        )?;
        report.count(status);
    }

    tx.commit().map_err(StorageError::PostgresClient)?;
    report.installed = true;
    report.generation = active_generation;
    report.schema_version = schema_version;
    Ok(report)
}

fn read_app_references<C: GenericClient>(
    client: &mut C,
    corpus: &str,
) -> Result<Vec<AppReference>, StorageError> {
    let rows = client
        .query(
            "SELECT reference_id, target_kind, corpus, document_id, source, source_uid, \
                    version_group, as_of_date::text AS as_of_date, resolved_document_id \
             FROM jurisearch_app.app_reference WHERE corpus = $1 ORDER BY reference_id;",
            &[&corpus],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(rows
        .into_iter()
        .map(|row| AppReference {
            reference_id: row.get("reference_id"),
            target_kind: row.get("target_kind"),
            corpus: row.get("corpus"),
            document_id: row.get("document_id"),
            source: row.get("source"),
            source_uid: row.get("source_uid"),
            version_group: row.get("version_group"),
            as_of_date: row.get("as_of_date"),
            prior_resolved_document_id: row.get("resolved_document_id"),
        })
        .collect())
}

/// Resolve one reference against `schema` (the active generation's physical schema). Returns the
/// resolved `document_id` (if any) + the [`ValidationStatus`].
fn resolve_reference<C: GenericClient>(
    client: &mut C,
    schema: &str,
    reference: &AppReference,
) -> Result<(Option<String>, ValidationStatus), StorageError> {
    match reference.target_kind.as_str() {
        "document_version" => match &reference.document_id {
            Some(document_id) => Ok(resolve_pin(client, schema, &reference.corpus, document_id)?),
            None => Ok((None, ValidationStatus::Invalid)),
        },
        "decision" => {
            if let Some(document_id) = &reference.document_id {
                Ok(resolve_pin(client, schema, &reference.corpus, document_id)?)
            } else if let (Some(source), Some(source_uid)) =
                (&reference.source, &reference.source_uid)
            {
                // A decision is not a temporal version; a `(source, source_uid)` repair path picks the
                // newest matching document (no validity window).
                Ok(resolve_decision_repair(
                    client,
                    schema,
                    &reference.corpus,
                    source,
                    source_uid,
                )?)
            } else {
                Ok((None, ValidationStatus::Invalid))
            }
        }
        "logical_article" => {
            let has_identity = reference.version_group.is_some() || reference.source_uid.is_some();
            match (&reference.as_of_date, has_identity) {
                (Some(as_of), true) => {
                    let resolved = resolve_logical(client, schema, reference, as_of)?;
                    Ok(classify_logical(reference, resolved))
                }
                _ => Ok((None, ValidationStatus::Invalid)),
            }
        }
        // Chunk / zone units anchor at the PARENT document level in P8 (derived identities are not
        // pinned). Validate the parent: a `document_id` pin, else logical-article identity.
        "chunk" | "zone_unit" => {
            if let Some(document_id) = &reference.document_id {
                Ok(resolve_pin(client, schema, &reference.corpus, document_id)?)
            } else if reference.as_of_date.is_some()
                && (reference.version_group.is_some() || reference.source_uid.is_some())
            {
                let as_of = reference.as_of_date.as_deref().expect("checked");
                let resolved = resolve_logical(client, schema, reference, as_of)?;
                Ok(classify_logical(reference, resolved))
            } else {
                Ok((None, ValidationStatus::Invalid))
            }
        }
        _ => Ok((None, ValidationStatus::Invalid)),
    }
}

/// A pin: the immutable `document_id` either exists in this generation or it does not. A generation /
/// schema stamp changing underneath a pin is NOT a target change, so a found pin is always `resolved`.
fn resolve_pin<C: GenericClient>(
    client: &mut C,
    schema: &str,
    corpus: &str,
    document_id: &str,
) -> Result<(Option<String>, ValidationStatus), StorageError> {
    let found = client
        .query_opt(
            &format!(
                "SELECT document_id FROM {}.documents WHERE document_id = $1 AND corpus = $2;",
                sql_identifier(schema)
            ),
            &[&document_id, &corpus],
        )
        .map_err(StorageError::PostgresClient)?;
    match found {
        Some(_) => Ok((Some(document_id.to_owned()), ValidationStatus::Resolved)),
        None => Ok((None, ValidationStatus::Missing)),
    }
}

fn resolve_decision_repair<C: GenericClient>(
    client: &mut C,
    schema: &str,
    corpus: &str,
    source: &str,
    source_uid: &str,
) -> Result<(Option<String>, ValidationStatus), StorageError> {
    let found = client
        .query_opt(
            &format!(
                "SELECT document_id FROM {}.documents \
                 WHERE corpus = $1 AND source = $2 AND source_uid = $3 \
                 ORDER BY valid_from DESC NULLS LAST, document_id LIMIT 1;",
                sql_identifier(schema)
            ),
            &[&corpus, &source, &source_uid],
        )
        .map_err(StorageError::PostgresClient)?;
    match found {
        Some(row) => Ok((
            Some(row.get::<_, String>("document_id")),
            ValidationStatus::Resolved,
        )),
        None => Ok((None, ValidationStatus::Missing)),
    }
}

/// Logical-article resolution by HALF-OPEN validity window (`valid_from <= as_of < valid_to`), matching
/// on `version_group` (falling back to `source_uid`), scoped by `source` when present. Newest
/// `valid_from` wins ties (design §8.3 / the existing retrieval temporal convention).
fn resolve_logical<C: GenericClient>(
    client: &mut C,
    schema: &str,
    reference: &AppReference,
    as_of: &str,
) -> Result<Option<String>, StorageError> {
    let found = client
        .query_opt(
            &format!(
                "SELECT document_id FROM {}.documents d \
                 WHERE d.corpus = $1 \
                   AND ($2::text IS NULL OR d.source = $2) \
                   AND ( ($3::text IS NOT NULL AND d.version_group = $3) \
                         OR ($3::text IS NULL AND d.source_uid = $4) ) \
                   AND (d.valid_from IS NULL OR d.valid_from <= $5::text::date) \
                   AND (d.valid_to IS NULL OR $5::text::date < d.valid_to) \
                 ORDER BY d.valid_from DESC NULLS LAST, d.document_id LIMIT 1;",
                sql_identifier(schema)
            ),
            &[
                &reference.corpus,
                &reference.source,
                &reference.version_group,
                &reference.source_uid,
                &as_of,
            ],
        )
        .map_err(StorageError::PostgresClient)?;
    Ok(found.map(|row| row.get::<_, String>("document_id")))
}

/// Classify a logical resolution: `missing` if none; `changed` if a PRIOR non-null resolution now
/// resolves to a different `document_id`; otherwise `resolved` (incl. the first validation).
fn classify_logical(
    reference: &AppReference,
    resolved: Option<String>,
) -> (Option<String>, ValidationStatus) {
    match resolved {
        None => (None, ValidationStatus::Missing),
        Some(document_id) => {
            let status = match &reference.prior_resolved_document_id {
                Some(prior) if *prior != document_id => ValidationStatus::Changed,
                _ => ValidationStatus::Resolved,
            };
            (Some(document_id), status)
        }
    }
}

/// Narrow write set (design §8.2): ONLY the `resolved_*` / `validated_at` / `validation_status` columns.
fn update_reference<C: GenericClient>(
    client: &mut C,
    reference_id: i64,
    resolved_document_id: Option<&str>,
    resolved_generation: Option<&str>,
    resolved_schema_version: Option<i32>,
    status: ValidationStatus,
) -> Result<(), StorageError> {
    client
        .execute(
            "UPDATE jurisearch_app.app_reference \
             SET resolved_document_id = $2, resolved_generation = $3, resolved_schema_version = $4, \
                 validated_at = now(), validation_status = $5 \
             WHERE reference_id = $1;",
            &[
                &reference_id,
                &resolved_document_id,
                &resolved_generation,
                &resolved_schema_version,
                &status.as_str(),
            ],
        )
        .map(|_| ())
        .map_err(StorageError::PostgresClient)?;
    Ok(())
}
