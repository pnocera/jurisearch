//! Enrich seam (S5): official Judilibre zone backfill over a [`DbClientSource`].
//!
//! [`enrich_zones`] pages the resolver-reachable Cassation candidate set and resolves each decision via
//! the shipped enrichment core, writing a `decision_zones` row per attempt (resumable). Unlike the CLI
//! payload — which ERRORED when credentials were absent — the library returns the honest
//! [`EnrichmentMode::SkippedNoCredentials`] so a producer cycle records "requested but skipped" rather
//! than silently claiming enrichment ran. The thin CLI consumer maps that back to its historical error.

use crate::*;

/// The honest enrichment outcome recorded by a producer cycle (mirrors the `enrich-zones` half of
/// `jurisearch_package_build::EnrichmentMode`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrichmentMode {
    /// Enrichment ran and refreshed `zones_enriched` decisions (the official-zone count).
    Ran { zones_enriched: u64 },
    /// Enrichment was requested but no Judilibre (PISTE) credentials were supplied — recorded, not run.
    SkippedNoCredentials,
}

/// Inputs for one [`enrich_zones`] pass. `source` must be `cass` or `inca` (Judilibre covers only the
/// Cour de cassation).
#[derive(Debug, Clone)]
pub struct EnrichRequest<'a> {
    pub source: &'a str,
    pub limit: Option<u32>,
    pub since: Option<&'a str>,
    pub concurrency: usize,
    pub order: EnrichZoneOrder,
}

/// What one zone-enrichment pass produced. `mode` is the honest credentials contract; the counts are
/// `0` and `coverage`/`body` minimal on the skipped path.
#[derive(Debug, Clone)]
pub struct EnrichOutcome {
    pub mode: EnrichmentMode,
    pub source: String,
    pub considered: u64,
    pub official_ok: u64,
    pub fallback: u64,
    pub errors: u64,
    pub coverage: Value,
    /// The historical `ingest enrich-zones` payload body (minus `index_dir`, which the CLI injects).
    pub body: Value,
}

/// Outcome of a single decision enrichment attempt, for backfill accounting.
#[derive(Clone, Copy)]
pub(crate) enum ZoneEnrichOutcome {
    Official,
    Fallback,
    Error,
}

fn order_label(order: EnrichZoneOrder) -> &'static str {
    match order {
        EnrichZoneOrder::Oldest => "oldest",
        EnrichZoneOrder::Recent => "recent",
    }
}

/// Eagerly backfill official Judilibre zones for a Cassation source (`cass`/`inca`) into
/// `decision_zones` over `db`. With no `piste` credentials this is a no-op that returns
/// [`EnrichmentMode::SkippedNoCredentials`] (honest, recorded).
///
/// # Errors
/// [`EnrichError`] for an invalid `source`, or a storage/JSON failure during paging.
pub fn enrich_zones(
    db: &impl DbClientSource,
    piste: Option<&PisteClient>,
    req: EnrichRequest<'_>,
) -> Result<EnrichOutcome, EnrichError> {
    enrich_zones_inner(db, piste, req).map_err(EnrichError::from)
}

fn enrich_zones_inner(
    db: &impl DbClientSource,
    piste: Option<&PisteClient>,
    req: EnrichRequest<'_>,
) -> Result<EnrichOutcome, ErrorObject> {
    let EnrichRequest {
        source,
        limit,
        since,
        concurrency,
        order,
    } = req;
    if !matches!(source, "cass" | "inca") {
        return Err(ErrorObject::bad_input(
            "ingest enrich-zones --source must be 'cass' or 'inca' (Judilibre covers only Cour de cassation)",
        ));
    }

    // Honest credentials contract: no PISTE client => recorded skip, never a fabricated "ran".
    let Some(piste) = piste else {
        let body = json!({
            "schema_version": SCHEMA_VERSION,
            "command": "ingest enrich-zones",
            "source": source,
            "since": since,
            "concurrency": concurrency,
            "order": order_label(order),
            "enrichment_mode": "skipped_no_credentials",
            "considered": 0,
            "official_ok": 0,
            "fallback": 0,
            "errors": 0,
        });
        return Ok(EnrichOutcome {
            mode: EnrichmentMode::SkippedNoCredentials,
            source: source.to_owned(),
            considered: 0,
            official_ok: 0,
            fallback: 0,
            errors: 0,
            coverage: Value::Null,
            body,
        });
    };

    let mut main_client = db.client().map_err(storage_error_object)?;
    let run_id = producer_run_id("enrich-zones");
    let outbox = jurisearch_storage::outbox::OutboxContext::new(
        &run_id,
        jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
    );

    let mut considered: u64 = 0;
    let mut official: u64 = 0;
    let mut fallback: u64 = 0;
    let mut errors: u64 = 0;
    let mut cursor: Option<String> = None;
    loop {
        // Respect --limit across pages.
        let page_limit = match limit {
            Some(limit) => {
                let done = u32::try_from(considered).unwrap_or(u32::MAX);
                if done >= limit {
                    break;
                }
                (limit - done).min(ENRICH_ZONES_PAGE_SIZE)
            }
            None => ENRICH_ZONES_PAGE_SIZE,
        };
        let page_json = enrich_zone_candidates_json_with_client(
            &mut main_client,
            source,
            cursor.as_deref(),
            since,
            page_limit,
            order,
        )
        .map_err(storage_error_object)?;
        let page: Value = serde_json::from_str(&page_json)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
        let doc_ids: Vec<String> = page["candidates"]
            .as_array()
            .map(|candidates| {
                candidates
                    .iter()
                    .filter_map(|candidate| candidate["document_id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if doc_ids.is_empty() {
            break;
        }
        for outcome in
            enrich_zone_page_concurrently(db, piste, &doc_ids, concurrency, Some(&outbox))
        {
            considered += 1;
            match outcome {
                ZoneEnrichOutcome::Official => official += 1,
                ZoneEnrichOutcome::Fallback => fallback += 1,
                ZoneEnrichOutcome::Error => errors += 1,
            }
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    let coverage: Value = serde_json::from_str(
        &zone_retrieval_coverage_with_client(&mut main_client).map_err(storage_error_object)?,
    )
    .map_err(|error| dependency_unavailable(error.to_string()))?;
    let body = json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest enrich-zones",
        "source": source,
        "since": since,
        "concurrency": concurrency,
        "order": order_label(order),
        "considered": considered,
        "official_ok": official,
        "fallback": fallback,
        "errors": errors,
        "coverage": coverage,
    });
    Ok(EnrichOutcome {
        mode: EnrichmentMode::Ran {
            zones_enriched: official,
        },
        source: source.to_owned(),
        considered,
        official_ok: official,
        fallback,
        errors,
        coverage,
        body,
    })
}

/// Enrich one page of decisions with bounded concurrency. Each worker gets its OWN producer client
/// (opened up front on the main thread and MOVED into the worker — `ManagedPostgres`/`postgres::Client`
/// is `Send` but not `Sync`, so the client source itself cannot be shared across threads) and resolves a
/// contiguous slice through the thread-safe enrichment core, sharing the read-only `piste` client. A
/// worker that cannot connect, or panics, drops only its slice from accounting (counted as errors)
/// rather than aborting the whole backfill.
pub(crate) fn enrich_zone_page_concurrently(
    db: &impl DbClientSource,
    piste: &PisteClient,
    doc_ids: &[String],
    concurrency: usize,
    outbox: Option<&jurisearch_storage::outbox::OutboxContext<'_>>,
) -> Vec<ZoneEnrichOutcome> {
    let workers = concurrency.max(1).min(doc_ids.len().max(1));
    let mut groups: Vec<Vec<&str>> = (0..workers).map(|_| Vec::new()).collect();
    for (index, doc_id) in doc_ids.iter().enumerate() {
        groups[index % workers].push(doc_id.as_str());
    }
    std::thread::scope(|scope| {
        let handles: Vec<(usize, _)> = groups
            .into_iter()
            .map(|group| {
                let group_len = group.len();
                // Open this worker's client on the main thread, then move the owned client in.
                let client = db.client();
                let handle = scope.spawn(move || {
                    let mut db_client = match client {
                        Ok(client) => client,
                        // Whole slice fails to connect -> count as errors, don't abort the run.
                        Err(_) => return vec![ZoneEnrichOutcome::Error; group.len()],
                    };
                    group
                        .into_iter()
                        .map(|doc_id| {
                            // Producer backfill: resolve the local metadata on this worker's own
                            // (`public`) connection — the producer's authoritative working set — and
                            // pass it into the enrichment core, which writes on the same connection.
                            let meta = match decision_resolution_metadata_with_client(
                                &mut db_client,
                                doc_id,
                            ) {
                                Ok(json) => match serde_json::from_str::<Value>(&json) {
                                    Ok(meta) => meta,
                                    Err(_) => return ZoneEnrichOutcome::Error,
                                },
                                Err(_) => return ZoneEnrichOutcome::Error,
                            };
                            match enrich_decision_from_judilibre_with_client(
                                &mut db_client,
                                piste,
                                doc_id,
                                &meta,
                                outbox,
                            ) {
                                Ok(Some(_)) => ZoneEnrichOutcome::Official,
                                Ok(None) => ZoneEnrichOutcome::Fallback,
                                Err(_) => ZoneEnrichOutcome::Error,
                            }
                        })
                        .collect::<Vec<_>>()
                });
                (group_len, handle)
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|(group_len, handle)| {
                worker_outcomes_or_errors(handle.join().ok(), group_len)
            })
            .collect()
    })
}

/// Map a scoped worker's join result to per-decision outcomes. A panicked worker (join `None`) counts
/// its WHOLE slice as errors rather than silently dropping those decisions from the backfill accounting.
pub(crate) fn worker_outcomes_or_errors(
    returned: Option<Vec<ZoneEnrichOutcome>>,
    group_len: usize,
) -> Vec<ZoneEnrichOutcome> {
    returned.unwrap_or_else(|| vec![ZoneEnrichOutcome::Error; group_len])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_join_error_counts_whole_slice_as_errors() {
        // A panicked backfill worker (join -> None) must count its whole slice as errors, not silently
        // drop those decisions from accounting.
        let panicked = worker_outcomes_or_errors(None, 3);
        assert_eq!(panicked.len(), 3);
        assert!(
            panicked
                .iter()
                .all(|outcome| matches!(outcome, ZoneEnrichOutcome::Error))
        );
        let returned = vec![ZoneEnrichOutcome::Official, ZoneEnrichOutcome::Fallback];
        assert_eq!(worker_outcomes_or_errors(Some(returned), 2).len(), 2);
    }
}
