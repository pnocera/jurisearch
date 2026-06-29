//! Judilibre official-zone enrichment + decision_zones overlay cache (zone_cache_action, enrich-from-Judilibre, TTL caching).

use crate::*;

/// Sources Judilibre can resolve by pourvoi+date: both PUBLISHED (`cass`) and INÉDIT (`inca`) Cour de
/// cassation decisions (verified live — inédit decisions resolve with `publication=[]` and return
/// official zones). NOT `capp` (Cour d'appel uses RG numbers, not resolvable on Judilibre here) and
/// NOT `jade` (administrative; Judilibre does not cover it).
pub fn is_judilibre_cassation_source(source: Option<&str>) -> bool {
    matches!(source, Some("cass" | "inca"))
}

/// The Judilibre `zones` key that backs a requested part, or `None` for parts not served by an
/// official zone offset (summary stays SOMMAIRE; visa has no primary Judilibre zone).
pub fn judilibre_zone_key(part: DecisionPart) -> Option<&'static str> {
    match part {
        DecisionPart::Motivations => Some("motivations"),
        DecisionPart::Moyens => Some("moyens"),
        DecisionPart::Dispositif => Some("dispositif"),
        DecisionPart::Summary | DecisionPart::Visa => None,
    }
}

/// What to do for a requested part given its cached `decision_zones` row (pure, so the cache/TTL
/// policy is unit-testable). A FRESH `ok` row serves the part (or falls back if that zone is empty);
/// a FRESH negative row suppresses network for its TTL; a missing/expired row triggers enrichment when
/// `--online` and the source is Cassation.
#[derive(Debug)]
pub enum ZoneCacheAction {
    Official(Value),
    Fallback,
    Enrich,
}

pub fn zone_cache_action(
    cached: &Value,
    part: DecisionPart,
    zone_key: &str,
    online: bool,
    source: &str,
) -> ZoneCacheAction {
    let expired = cached["expired"].as_bool() == Some(true);
    match cached["status"].as_str() {
        Some("ok") if !expired => match part_block_from_cached_zones(cached, part, zone_key) {
            Some(block) => ZoneCacheAction::Official(block),
            None => ZoneCacheAction::Fallback,
        },
        Some(_) if !expired => ZoneCacheAction::Fallback,
        // status is null (no row) or the row is expired -> enrich when we can, else fall back.
        _ if online && is_judilibre_cassation_source(Some(source)) => ZoneCacheAction::Enrich,
        _ => ZoneCacheAction::Fallback,
    }
}

/// Build the official-part response block from a cached zones row, or `None` if that part has no
/// non-empty official zone.
pub fn part_block_from_cached_zones(
    cached: &Value,
    part: DecisionPart,
    zone_key: &str,
) -> Option<Value> {
    let fragments = cached["zones"][zone_key].as_array()?;
    if fragments.is_empty() {
        return None;
    }
    let text = fragments
        .iter()
        .filter_map(|fragment| fragment["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.trim().is_empty() {
        return None;
    }
    Some(json!({
        "requested": part.name(),
        "applicable": true,
        "available": true,
        "official_zones": true,
        "zone_accurate": true,
        "zone_provenance": "judilibre",
        "provider": cached["provider"].clone(),
        "provider_decision_id": cached["provider_decision_id"].clone(),
        "fetched_at": cached["fetched_at"].clone(),
        "upstream_update_date": cached["upstream_update_date"].clone(),
        "fragments": Value::Array(fragments.clone()),
        "text": text,
        "note": "Official Judilibre zone offsets (character indices) for this Cour de cassation decision."
    }))
}

pub fn enrich_decision_from_judilibre(
    postgres: &ManagedPostgres,
    document_id: &str,
) -> Result<Option<Value>, ErrorObject> {
    // Resolve the local resolution metadata through the CLIENT READ ROLE (active generation), not the
    // raw write connection's default `public`. On a generation-backed client whose `public` is empty,
    // resolving the pourvoi/ECLI/source_uid against `public` would fail and `fetch --part --online`
    // would wrongly cache the decision as unsupported (the r4 split-brain). The producer-style writes in
    // the core still go to the caller-owned `public` connection below.
    let meta: Value = serde_json::from_str(
        &decision_resolution_metadata_json(postgres, document_id).map_err(storage_error_object)?,
    )
    .map_err(|error| dependency_unavailable(error.to_string()))?;
    let mut db = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
    let piste = PisteClient::new(OfficialApiConfig::from_env());
    // On-demand single-decision enrichment (the client-facing `fetch --part --online` path): not a
    // producer capture run, so no outbox emission. Scheduled producer capture goes through the batch
    // `enrich-zones` command, which threads an OutboxContext.
    enrich_decision_from_judilibre_with_client(&mut db, &piste, document_id, &meta, None)
}

/// Thread-safe enrichment core: takes a caller-owned `postgres::Client` and `PisteClient` (no
/// `&ManagedPostgres`), so eager-backfill workers can each hold their own connections. Identical
/// behaviour to the wrapper above.
pub fn enrich_decision_from_judilibre_with_client<C: postgres::GenericClient>(
    db: &mut C,
    piste: &PisteClient,
    document_id: &str,
    resolution_meta: &Value,
    outbox: Option<&jurisearch_storage::outbox::OutboxContext<'_>>,
) -> Result<Option<Value>, ErrorObject> {
    // Local resolution metadata (parser-valid pourvoi, decision date = valid_from, source_uid) is
    // resolved by the CALLER and passed in: the client-facing wrapper resolves it through the read role
    // (active generation), while the producer backfill resolves it on its own `public` connection. The
    // writes below stay on the caller-owned `db`.
    let meta = resolution_meta;

    let source_uid = meta["source_uid"].as_str().unwrap_or_default();
    let ecli = meta["ecli"].as_str();
    let decision_date = meta["decision_date"].as_str();
    let api_environment = piste.api_environment();
    // Each producer write below + its outbox emit run in one transaction (P1 §5.1: emit is
    // rollback-coupled to the mutation). HTTP happens OUTSIDE the transactions.
    let cache = |db: &mut C, status: &str, error: Option<&str>| -> Result<(), ErrorObject> {
        in_outbox_txn(db, |tx| {
            cache_zone_status_with_client(
                tx,
                document_id,
                source_uid,
                ecli,
                decision_date,
                status,
                error,
                outbox,
            )
        })
    };

    let Some(pourvoi) = meta["pourvoi"].as_str() else {
        // No parser-valid pourvoi -> cannot even request Judilibre. Archive a durable 'local' accounting
        // row (so every touched decision is recorded) and cache as unsupported — one transaction.
        in_outbox_txn(db, |tx| {
            archive_local_unsupported(tx, document_id, source_uid, api_environment, outbox)?;
            cache_zone_status_with_client(
                tx,
                document_id,
                source_uid,
                ecli,
                decision_date,
                "unsupported",
                None,
                outbox,
            )
        })?;
        return Ok(None);
    };

    let normalized_pourvoi: String = pourvoi
        .chars()
        .filter(|c| !matches!(c, '.' | ' '))
        .collect();

    // Resolve: search by pourvoi (free-text exact); accept the result whose normalized number matches
    // and whose date matches when we have one. Archive the raw /search response either way.
    let search_exchange = piste.judilibre_search_params_exchange(&[
        ("query", pourvoi),
        ("operator", "exact"),
        ("page_size", "10"),
    ]);
    in_outbox_txn(db, |tx| {
        archive_exchange(
            tx,
            &search_exchange,
            api_environment,
            Some(document_id),
            Some(source_uid),
            None,
            None,
            None,
            outbox,
        )
        .map(|_| ())
    })?;
    if search_exchange.outcome != OfficialApiOutcome::Ok {
        cache(db, "upstream_error", search_exchange.error.as_deref())?;
        return Ok(None);
    }
    let provider_id = search_exchange
        .response_json
        .as_ref()
        .and_then(|search| find_matching_judilibre_id(search, &normalized_pourvoi, decision_date));
    let Some(provider_id) = provider_id else {
        cache(db, "not_found", None)?;
        return Ok(None);
    };

    // Fetch the full decision; archive the raw /decision response either way.
    let decision_exchange = piste.judilibre_decision_exchange(&provider_id, None);
    in_outbox_txn(db, |tx| {
        archive_exchange(
            tx,
            &decision_exchange,
            api_environment,
            Some(document_id),
            Some(source_uid),
            Some(provider_id.as_str()),
            None,
            None,
            outbox,
        )
        .map(|_| ())
    })?;
    if decision_exchange.outcome != OfficialApiOutcome::Ok {
        cache(db, "upstream_error", decision_exchange.error.as_deref())?;
        return Ok(None);
    }
    let Some(decision) = decision_exchange.response_json.as_ref() else {
        cache(
            db,
            "upstream_error",
            Some("decision response missing JSON body"),
        )?;
        return Ok(None);
    };

    let (zones_json, valid_zones) = normalize_judilibre_zones(decision);
    let status = if valid_zones { "ok" } else { "invalid_offsets" };
    // Deterministic content hash over the resolved snapshot (Judilibre text + normalized zones +
    // provider id + update date). Set for ok/invalid_offsets rows so eager backfill rows are derivable
    // into zone_units and refreshes can detect change; negative rows (cache_zone_status) stay hashless.
    let text_hash = zone_text_hash(decision, &zones_json, provider_id.as_str());
    let ttl_days: i64 = env_i64("JURISEARCH_JUDILIBRE_ZONE_TTL_DAYS", 30);
    let row = UpsertDecisionZones {
        document_id,
        provider: "judilibre",
        provider_decision_id: Some(provider_id.as_str()),
        source_uid,
        ecli,
        status,
        upstream_update_date: decision["update_date"].as_str(),
        upstream_decision_date: decision["decision_date"].as_str(),
        text_hash: Some(text_hash.as_str()),
        offset_unit: Some("char"),
        zones_json: &zones_json,
        raw_json: decision,
        error: None,
        ttl_seconds: Some(ttl_days.max(0) * 86_400),
    };
    in_outbox_txn(db, |tx| {
        upsert_decision_zones_with_client(tx, &row, outbox).map_err(storage_error_object)
    })?;
    if status != "ok" {
        return Ok(None);
    }
    // Return a cached-shaped value so the caller can read the part it asked for.
    Ok(Some(json!({
        "status": "ok",
        "provider": "judilibre",
        "provider_decision_id": provider_id,
        "upstream_update_date": decision["update_date"].clone(),
        "zones": zones_json,
    })))
}

/// Pick the Judilibre search result whose normalized `numbers` contains the local pourvoi and (when
/// available) whose `decision_date` matches — guarding against pourvoi collisions across years.
pub fn find_matching_judilibre_id(
    search: &Value,
    normalized_pourvoi: &str,
    decision_date: Option<&str>,
) -> Option<String> {
    let results = search["results"].as_array()?;
    let normalize =
        |value: &str| -> String { value.chars().filter(|c| !matches!(c, '.' | ' ')).collect() };
    let mut date_agnostic: Option<String> = None;
    for result in results {
        let numbers_match = result["numbers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|number| number.as_str())
            .any(|number| normalize(number) == normalized_pourvoi)
            || result["number"].as_str().map(normalize).as_deref() == Some(normalized_pourvoi);
        if !numbers_match {
            continue;
        }
        let Some(id) = result["id"].as_str() else {
            continue;
        };
        match (decision_date, result["decision_date"].as_str()) {
            // Local date known: require an exact remote-date match — never resolve by number alone
            // (guards pourvoi collisions across years even if a result is missing its date).
            (Some(local), Some(remote)) if local == remote => return Some(id.to_owned()),
            (Some(_), _) => continue,
            // No local date: accept the first number match as a date-agnostic fallback.
            (None, _) => {
                date_agnostic.get_or_insert_with(|| id.to_owned());
            }
        };
    }
    date_agnostic
}

/// Normalize Judilibre `zones` (character-index offsets into `text`) into
/// `{motivations:[{start,end,text}], moyens:[…], dispositif:[…]}`. Returns `(zones_json, any_valid)`.
/// Offsets are CHARACTER indices (verified against the live API), so slicing is char-safe.
pub fn normalize_judilibre_zones(decision: &Value) -> (Value, bool) {
    let text_chars: Vec<char> = decision["text"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .collect();
    let mut zones = serde_json::Map::new();
    let mut any_valid = false;
    for key in ["motivations", "moyens", "dispositif"] {
        let mut fragments = Vec::new();
        if let Some(offsets) = decision["zones"][key].as_array() {
            for offset in offsets {
                let (Some(start), Some(end)) = (offset["start"].as_u64(), offset["end"].as_u64())
                else {
                    continue;
                };
                let (start, end) = (start as usize, end as usize);
                if start > end || end > text_chars.len() {
                    continue; // out-of-range -> skip this fragment (offset_unit mismatch guard)
                }
                let fragment_text: String = text_chars[start..end].iter().collect();
                if fragment_text.trim().is_empty() {
                    continue;
                }
                any_valid = true;
                fragments.push(json!({ "start": start, "end": end, "text": fragment_text }));
            }
        }
        zones.insert(key.to_owned(), Value::Array(fragments));
    }
    (Value::Object(zones), any_valid)
}

/// Deterministic content hash identifying a resolved zone snapshot, stored as `decision_zones.text_hash`
/// so derivation (`zone_units`) and refresh can detect change. Stable over the Judilibre `text`, the
/// normalized zones, the provider decision id, and the upstream `update_date` (NUL-separated so field
/// boundaries can't collide). Same `sha256:<hex>` shape as the ingest `source_payload_hash`.
pub fn zone_text_hash(decision: &Value, zones_json: &Value, provider_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(decision["text"].as_str().unwrap_or_default().as_bytes());
    hasher.update([0u8]);
    hasher.update(zones_json.to_string().as_bytes());
    hasher.update([0u8]);
    hasher.update(provider_id.as_bytes());
    hasher.update([0u8]);
    hasher.update(
        decision["update_date"]
            .as_str()
            .unwrap_or_default()
            .as_bytes(),
    );
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("sha256:{hex}")
}

/// Cache a non-`ok` zone status (unsupported / not_found / upstream_error) so repeat fetches do not
/// re-hit the API. Negative results get the negative TTL; upstream errors are not cached long.
/// Client-based core so backfill workers reuse their own connection.
#[allow(clippy::too_many_arguments)]
pub fn cache_zone_status_with_client<C: postgres::GenericClient>(
    db: &mut C,
    document_id: &str,
    source_uid: &str,
    ecli: Option<&str>,
    decision_date: Option<&str>,
    status: &str,
    error: Option<&str>,
    outbox: Option<&jurisearch_storage::outbox::OutboxContext<'_>>,
) -> Result<(), ErrorObject> {
    let ttl_seconds = match status {
        // Transient failures get a SHORT explicit TTL so they suppress hammering but retry soon (never
        // a permanent NULL-expiry row).
        "upstream_error" => {
            Some(env_i64("JURISEARCH_JUDILIBRE_ZONE_ERROR_TTL_SECONDS", 3600).max(0))
        }
        _ => Some(env_i64("JURISEARCH_JUDILIBRE_ZONE_NEGATIVE_TTL_DAYS", 7).max(0) * 86_400),
    };
    let empty = json!({});
    let row = UpsertDecisionZones {
        document_id,
        provider: "judilibre",
        provider_decision_id: None,
        source_uid,
        ecli,
        status,
        upstream_update_date: None,
        upstream_decision_date: decision_date,
        text_hash: None,
        offset_unit: None,
        zones_json: &empty,
        raw_json: &empty,
        error,
        ttl_seconds,
    };
    upsert_decision_zones_with_client(db, &row, outbox).map_err(storage_error_object)
}

pub fn env_i64(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(default)
}
