//! Decision-part fetch overlay: parse DecisionPart, annotate fetched parts, heuristic extraction (visa/dispositif/summary).

use crate::*;

/// A named jurisprudence-decision part requested via `fetch --part`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionPart {
    Summary,
    Visa,
    Dispositif,
    Motivations,
    Moyens,
}

impl DecisionPart {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "summary" | "sommaire" => Some(Self::Summary),
            "visa" => Some(Self::Visa),
            "dispositif" => Some(Self::Dispositif),
            "motivations" | "motivation" => Some(Self::Motivations),
            "moyens" | "moyen" => Some(Self::Moyens),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Visa => "visa",
            Self::Dispositif => "dispositif",
            Self::Motivations => "motivations",
            Self::Moyens => "moyens",
        }
    }
}

/// Attach a `part` block to each fetched document. For non-decision documents the part is
/// `not_applicable`. DILA bulk decisions have NO official Judilibre zones, so `summary` comes from the
/// `SOMMAIRE` chunk and every other part is best-effort `heuristic` (or `unavailable`) — never claimed
/// as an official zone. Each part reports `zone_provenance` and `official_zones: false`.
pub fn annotate_fetched_parts(
    postgres: &ManagedPostgres,
    response: &mut Value,
    part: DecisionPart,
    online: bool,
) -> Result<(), ErrorObject> {
    // Collect (document_id, source) for decisions first so the official-zone lookups don't fight the
    // mutable borrow of the documents array.
    let decisions: Vec<(String, String)> = response["documents"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|document| document["kind"].as_str() == Some("decision"))
        .map(|document| {
            (
                document["document_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                document["source"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    // Look up each DISTINCT decision once (fetch preserves duplicate requested IDs), then apply the
    // result to every matching copy below — so duplicate IDs get identical `part` blocks.
    let mut official: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    let mut looked_up: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (document_id, source) in &decisions {
        if !looked_up.insert(document_id.as_str()) {
            continue;
        }
        if let Some(block) = official_decision_part(postgres, document_id, source, part, online)? {
            official.insert(document_id.clone(), block);
        }
    }

    let Some(documents) = response["documents"].as_array_mut() else {
        return Ok(());
    };
    for document in documents {
        let is_decision = document["kind"].as_str() == Some("decision");
        if !is_decision {
            document["part"] = json!({
                "requested": part.name(),
                "applicable": false,
                "note": "fetch --part applies to jurisprudence decisions, not this document kind."
            });
            continue;
        }
        let document_id = document["document_id"].as_str().unwrap_or_default();
        // `get().cloned()` (not `remove`) so duplicate requested IDs each receive the official block.
        if let Some(block) = official.get(document_id).cloned() {
            document["part"] = block;
            continue;
        }
        // Fallback: source SOMMAIRE / heuristic / unavailable (no official zones).
        let body = document["body"].as_str().unwrap_or_default();
        let summary = collect_decision_summary(document);
        let extracted = extract_decision_part(part, body, summary.as_deref());
        // Whether an official zone COULD be obtained for this part with --online (judicial zone parts).
        let online_available = judilibre_zone_key(part).is_some()
            && is_judilibre_cassation_source(document["source"].as_str());
        document["part"] = json!({
            "requested": part.name(),
            "applicable": true,
            "official_zones": false,
            "zone_provenance": extracted.provenance,
            "available": extracted.text.is_some(),
            "text": extracted.text,
            "note": extracted.note,
            "official_zones_available": online_available && !online,
            "official_zones_hint": if online_available && !online {
                json!("re-run with --online to fetch the official Judilibre zone for this Cour de cassation decision")
            } else {
                Value::Null
            }
        });
    }
    Ok(())
}

/// Return an official-zone part block for a decision when available — from the `decision_zones` cache,
/// or (when `online`) by resolving the decision on Judilibre and caching the result. `None` means "no
/// official zone; use the heuristic/unavailable fallback". A transient upstream failure is cached and
/// returns `None` (it never fails the whole `fetch`).
pub fn official_decision_part(
    postgres: &ManagedPostgres,
    document_id: &str,
    source: &str,
    part: DecisionPart,
    online: bool,
) -> Result<Option<Value>, ErrorObject> {
    let Some(zone_key) = judilibre_zone_key(part) else {
        return Ok(None);
    };
    let cached: Value = serde_json::from_str(
        &decision_zones_json(postgres, document_id).map_err(storage_error_object)?,
    )
    .map_err(|error| dependency_unavailable(error.to_string()))?;
    match zone_cache_action(&cached, part, zone_key, online, source) {
        // Fresh cache row honored without network (a fresh `ok` row already holds every zone, so an
        // absent part is genuinely "no official zone", not a reason to re-fetch; a fresh negative row
        // suppresses repeat lookups for its TTL).
        ZoneCacheAction::Official(block) => Ok(Some(block)),
        ZoneCacheAction::Fallback => Ok(None),
        // No cache row, or an expired one: (re)fetch from Judilibre.
        ZoneCacheAction::Enrich => {
            let enriched = enrich_decision_from_judilibre(postgres, document_id)?;
            Ok(enriched.and_then(|cached| part_block_from_cached_zones(&cached, part, zone_key)))
        }
    }
}

pub struct ExtractedPart {
    pub text: Option<String>,
    pub provenance: &'static str,
    pub note: &'static str,
}

pub fn collect_decision_summary(document: &Value) -> Option<String> {
    let chunks = document["chunks"].as_array()?;
    let summary = chunks
        .iter()
        .filter(|chunk| chunk["chunk_kind"].as_str() == Some("decision_summary"))
        .filter_map(|chunk| chunk["body"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!summary.trim().is_empty()).then_some(summary)
}

pub fn extract_decision_part(
    part: DecisionPart,
    body: &str,
    summary: Option<&str>,
) -> ExtractedPart {
    match part {
        // SOMMAIRE is a real (if not zone-offset) structural element of the source record.
        DecisionPart::Summary => ExtractedPart {
            text: summary.map(str::to_owned),
            provenance: "sommaire",
            note: "From the source SOMMAIRE (titrage/analyse); not a Judilibre zone offset.",
        },
        // The dispositif reliably follows a "PAR CES MOTIFS" / "DÉCIDE" marker in French decisions.
        DecisionPart::Dispositif => ExtractedPart {
            text: heuristic_dispositif(body),
            provenance: "heuristic",
            note: "Best-effort heuristic from a dispositif marker; not an official Judilibre zone.",
        },
        // The visa ("Vu …") opens many decisions.
        DecisionPart::Visa => ExtractedPart {
            text: heuristic_visa(body),
            provenance: "heuristic",
            note: "Best-effort heuristic from leading 'Vu …' lines; not an official Judilibre zone.",
        },
        // Motivations/moyens have no reliable bulk marker; honestly report unavailable rather than
        // returning an over-claimed segmentation.
        DecisionPart::Motivations | DecisionPart::Moyens => ExtractedPart {
            text: None,
            provenance: "unavailable",
            note: "No official zones in DILA bulk; this part needs Judilibre zone enrichment.",
        },
    }
}

/// Heuristic dispositif: text from the last dispositif marker to the end of the body. Markers are
/// matched against the ORIGINAL body (never via `to_uppercase`, which can shift byte offsets on
/// accented French text and mis-slice/panic); every offset is a valid UTF-8 byte index. ASCII markers
/// match case-insensitively (`DECIDE`/`Decide`/`decide`); the accented `Décide` form is matched
/// exactly in its common casings since `rfind_ascii_ci` only folds ASCII bytes.
pub fn heuristic_dispositif(body: &str) -> Option<String> {
    const ASCII_MARKERS: &[&str] = &["PAR CES MOTIFS", "D E C I D E", "DECIDE"];
    const ACCENTED_MARKERS: &[&str] = &["DÉCIDE", "Décide", "décide"];
    let start = ASCII_MARKERS
        .iter()
        .filter_map(|marker| rfind_ascii_ci(body, marker))
        .chain(
            ACCENTED_MARKERS
                .iter()
                .filter_map(|marker| body.rfind(marker)),
        )
        .max()?;
    let tail = body[start..].trim();
    (!tail.is_empty()).then(|| tail.to_owned())
}

/// Heuristic visa: the FIRST contiguous block of `Vu …` lines (the opening visa), skipping any header
/// lines before it and stopping at the first substantive non-`Vu` line. Restricting to the leading
/// block prevents a later reasoning/quoted line that happens to start with `Vu` from being returned.
pub fn heuristic_visa(body: &str) -> Option<String> {
    let mut visa = Vec::new();
    let mut started = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let upper = trimmed.to_uppercase();
        let is_vu = upper.starts_with("VU ") || upper.starts_with("VU :") || upper == "VU";
        if is_vu {
            started = true;
            visa.push(trimmed);
        } else if started {
            break; // first substantive line after the opening Vu block ends the visa
        }
    }
    (!visa.is_empty()).then(|| visa.join("\n"))
}
