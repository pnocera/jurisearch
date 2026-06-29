//! `site smoke` / `demo smoke` engine (plan `01` Phase 7/8, M5-B): prove an INSTALLED site answers
//! real status/fetch/search legs over the versioned site protocol, plus NEGATIVE checks — and report
//! EVERY leg with an explicit outcome (`ran/passed`, `ran/failed`, or `skipped-with-recorded-reason`).
//!
//! The acceptance gate "no smoke leg is silently skipped" is structural here: a leg can ONLY produce a
//! [`LegOutcome`], and the [`LegOutcome::Skipped`] variant CARRIES a non-empty reason (enforced by
//! [`SmokeReport::invariant_no_silent_skip`] + tests). The per-leg DECISION logic (what counts as a pass,
//! a fail, or an authorized skip) is PURE over an already-obtained `SessionResponse`/`ClientError`, so it
//! is fully unit-tested with synthetic responses — no live site, DB, or embedder required. The live
//! [`run_smoke`] path only dials the endpoint (via the thin `jurisearch-client`) and feeds the real
//! response into the same pure classifiers, so the tested decision and the operated decision are one.

use serde::Serialize;
use serde_json::{Value, json};

use jurisearch_client::{ClientError, SiteEndpoint, send_request, status_probe_request};
use jurisearch_core::error::ErrorCode;
use jurisearch_core::session::{SessionRequest, SessionResponse};

/// One smoke leg. Each leg maps to a DISTINCT stable `code` so a runbook/CI can pin individual legs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SmokeLeg {
    /// The narrowest site operation: a `status` handshake (connectivity + protocol + live service).
    Status,
    /// `fetch` the documented stable FIXTURE/real id — proves the corpus is applied and queryable.
    FetchKnownId,
    /// `search` with `mode=bm25` — the lexical retrieval path (needs no embedder).
    Bm25Search,
    /// `search` with `mode=hybrid` — needs the loopback query embedder + model/tokenizer assets.
    HybridSearch,
    /// NEGATIVE: `fetch` a guaranteed-absent id must return not-found, never a document.
    NegativeMissingId,
    /// NEGATIVE: a malformed/empty query must be HANDLED (bad-input or empty), never a crash.
    NegativeBadQuery,
}

impl SmokeLeg {
    /// The stable machine code for this leg.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            SmokeLeg::Status => "smoke.status",
            SmokeLeg::FetchKnownId => "smoke.fetch_known_id",
            SmokeLeg::Bm25Search => "smoke.bm25_search",
            SmokeLeg::HybridSearch => "smoke.hybrid_search",
            SmokeLeg::NegativeMissingId => "smoke.negative_missing_id",
            SmokeLeg::NegativeBadQuery => "smoke.negative_bad_query",
        }
    }
}

/// The EXPLICIT outcome of one leg. There is no "silently skipped" state: a skip is a first-class
/// variant that REQUIRES a recorded reason (the acceptance gate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum LegOutcome {
    /// The leg ran and passed.
    Passed { detail: String },
    /// The leg ran and failed (the site is reachable but answered wrong / not-ready / unreachable).
    Failed { detail: String },
    /// The leg was NOT run, with an explicit, recorded reason (e.g. hybrid when model/tokenizer assets
    /// are absent). A skip is never a pass and never a failure.
    Skipped { reason: String },
}

impl LegOutcome {
    #[must_use]
    pub fn passed(detail: impl Into<String>) -> Self {
        Self::Passed {
            detail: detail.into(),
        }
    }
    #[must_use]
    pub fn failed(detail: impl Into<String>) -> Self {
        Self::Failed {
            detail: detail.into(),
        }
    }
    #[must_use]
    pub fn skipped(reason: impl Into<String>) -> Self {
        Self::Skipped {
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn is_failed(&self) -> bool {
        matches!(self, LegOutcome::Failed { .. })
    }

    /// A skip MUST carry a non-empty reason — the structural guard behind "no silent skip".
    #[must_use]
    pub fn has_explicit_reason(&self) -> bool {
        match self {
            LegOutcome::Passed { detail } | LegOutcome::Failed { detail } => !detail.is_empty(),
            LegOutcome::Skipped { reason } => !reason.is_empty(),
        }
    }
}

/// One leg + its explicit outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegResult {
    pub leg: SmokeLeg,
    pub code: &'static str,
    #[serde(flatten)]
    pub outcome: LegOutcome,
}

impl LegResult {
    #[must_use]
    pub fn new(leg: SmokeLeg, outcome: LegOutcome) -> Self {
        Self {
            leg,
            code: leg.code(),
            outcome,
        }
    }
}

/// The full smoke report — one [`LegResult`] per PLANNED leg, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SmokeReport {
    pub legs: Vec<LegResult>,
}

impl SmokeReport {
    pub fn push(&mut self, leg: SmokeLeg, outcome: LegOutcome) {
        self.legs.push(LegResult::new(leg, outcome));
    }

    /// `true` when no leg FAILED (skips with a recorded reason do not fail the smoke).
    #[must_use]
    pub fn passed(&self) -> bool {
        !self.legs.iter().any(|leg| leg.outcome.is_failed())
    }

    /// `0` when no leg failed, else `1` — the process exit code.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        u8::from(!self.passed())
    }

    /// The structural "no silent skip" guarantee: EVERY leg carries an explicit outcome with a non-empty
    /// human reason/detail. Unit-tested, and asserted at runtime before the report is emitted.
    #[must_use]
    pub fn invariant_no_silent_skip(&self) -> bool {
        self.legs
            .iter()
            .all(|leg| leg.outcome.has_explicit_reason())
    }

    /// A stable machine view for runbooks/CI.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "passed": self.passed(),
            "legs": self.legs,
        })
    }

    /// Human lines, one per leg.
    #[must_use]
    pub fn to_lines(&self) -> String {
        let mut out = String::new();
        for leg in &self.legs {
            let (tag, text) = match &leg.outcome {
                LegOutcome::Passed { detail } => ("PASS", detail.as_str()),
                LegOutcome::Failed { detail } => ("FAIL", detail.as_str()),
                LegOutcome::Skipped { reason } => ("SKIP", reason.as_str()),
            };
            out.push_str(&format!("[{tag}] {} — {text}\n", leg.code));
        }
        out
    }
}

/// What the smoke run needs to know: the stable known id, a query term present in the corpus, a
/// guaranteed-absent id for the negative leg, and whether the HYBRID (embedder) leg is configured.
#[derive(Debug, Clone)]
pub struct SmokePlan {
    /// The documented stable FIXTURE/real document id (`fetch` known-id + the negative baseline).
    pub known_id: String,
    /// A query term expected to retrieve at least one candidate from the corpus.
    pub query_term: String,
    /// A guaranteed-absent id for the NEGATIVE not-found leg.
    pub missing_id: String,
    /// Whether the hybrid (loopback-embedder) leg should RUN. When `false`, the hybrid leg is recorded as
    /// a skip WITH the supplied [`hybrid_skip_reason`].
    pub hybrid_enabled: bool,
    /// The explicit reason recorded when `hybrid_enabled == false` (never a silent skip).
    pub hybrid_skip_reason: String,
}

impl SmokePlan {
    /// A plan with hybrid ENABLED (the site config has a loopback query embedder + assets).
    #[must_use]
    pub fn with_hybrid(
        known_id: impl Into<String>,
        query_term: impl Into<String>,
        missing_id: impl Into<String>,
    ) -> Self {
        Self {
            known_id: known_id.into(),
            query_term: query_term.into(),
            missing_id: missing_id.into(),
            hybrid_enabled: true,
            hybrid_skip_reason: String::new(),
        }
    }

    /// A plan with hybrid DISABLED — the explicit reason is recorded for the hybrid leg.
    #[must_use]
    pub fn without_hybrid(
        known_id: impl Into<String>,
        query_term: impl Into<String>,
        missing_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            known_id: known_id.into(),
            query_term: query_term.into(),
            missing_id: missing_id.into(),
            hybrid_enabled: false,
            hybrid_skip_reason: reason.into(),
        }
    }
}

// ---- request builders (the REAL site requests each leg sends) -------------------------------------

fn fetch_request(id: &str) -> SessionRequest {
    SessionRequest {
        id: Some(Value::String("jurisearchctl-smoke-fetch".to_owned())),
        command: "fetch".to_owned(),
        args: json!({ "ids": [id] }),
    }
}

fn search_request(query: &str, mode: &str) -> SessionRequest {
    SessionRequest {
        id: Some(Value::String("jurisearchctl-smoke-search".to_owned())),
        command: "search".to_owned(),
        args: json!({ "query": query, "mode": mode }),
    }
}

fn bad_query_request() -> SessionRequest {
    // An EMPTY query: the contract requires a `query` field; the site must reject it as bad-input (or
    // return an empty result) — never crash and never drop the connection.
    SessionRequest {
        id: Some(Value::String("jurisearchctl-smoke-badquery".to_owned())),
        command: "search".to_owned(),
        args: json!({ "query": "" }),
    }
}

// ---- pure response classifiers (unit-tested without a live site) ----------------------------------

type Outcome = Result<SessionResponse, ClientError>;

/// PURE: `status` passes when the service answered OK; a served error or a transport failure fails.
#[must_use]
pub fn classify_status(outcome: &Outcome) -> LegOutcome {
    match outcome {
        Ok(response) if response.is_ok() => {
            LegOutcome::passed("site answered `status` OK over the versioned site protocol")
        }
        Ok(response) => LegOutcome::failed(format!(
            "site reachable but `status` returned an error: {}",
            error_detail(response)
        )),
        Err(error) => LegOutcome::failed(format!("`status` could not be served: {error}")),
    }
}

/// Whether a `fetch` result body contains a document for `id` (the result shape is
/// `{ "documents": [ { "document_id": "…" }, … ] }`).
#[must_use]
pub fn fetch_returned_id(result: &Value, id: &str) -> bool {
    result
        .get("documents")
        .and_then(Value::as_array)
        .is_some_and(|docs| {
            docs.iter()
                .any(|doc| doc.get("document_id").and_then(Value::as_str) == Some(id))
        })
}

/// Whether a `fetch` result is the contract's not-found shape: a `documents` field that EXISTS and is an
/// ARRAY of length zero. A MISSING or NON-ARRAY `documents` is NOT this shape (a non-contract success body
/// like `{}` or `{ "candidates": [] }` is not a valid not-found) and must FAIL the negative leg.
#[must_use]
pub fn documents_is_empty_array(result: &Value) -> bool {
    result
        .get("documents")
        .and_then(Value::as_array)
        .is_some_and(|docs| docs.is_empty())
}

/// PURE: `fetch known-id` passes only when the document for the stable id comes back.
#[must_use]
pub fn classify_fetch_known(outcome: &Outcome, id: &str) -> LegOutcome {
    match outcome {
        Ok(response) => match response.result() {
            Some(result) if fetch_returned_id(result, id) => {
                LegOutcome::passed(format!("fetched the known document `{id}`"))
            }
            Some(_) => LegOutcome::failed(format!(
                "`fetch {id}` returned no matching document — is the fixture/real corpus applied?"
            )),
            None => LegOutcome::failed(format!(
                "`fetch {id}` returned an error: {}",
                error_detail(response)
            )),
        },
        Err(error) => LegOutcome::failed(format!("`fetch {id}` could not be served: {error}")),
    }
}

/// Whether a `search` result body carries at least one candidate (`{ "candidates": [ … ] }`).
#[must_use]
pub fn search_has_candidates(result: &Value) -> bool {
    result
        .get("candidates")
        .and_then(Value::as_array)
        .is_some_and(|candidates| !candidates.is_empty())
}

/// Whether a `search` result is the contract's clean-empty shape: a `candidates` field that EXISTS and is
/// an ARRAY of length zero. A MISSING or NON-ARRAY `candidates` is NOT this shape (a non-contract success
/// body like `{}` is not a valid clean-empty result) and must FAIL the bad-query negative leg.
#[must_use]
pub fn candidates_is_empty_array(result: &Value) -> bool {
    result
        .get("candidates")
        .and_then(Value::as_array)
        .is_some_and(|candidates| candidates.is_empty())
}

/// PURE: a positive `search` leg (bm25 or hybrid) passes when the site returns >= 1 candidate.
#[must_use]
pub fn classify_search(outcome: &Outcome, mode: &str, query: &str) -> LegOutcome {
    match outcome {
        Ok(response) => match response.result() {
            Some(result) if search_has_candidates(result) => {
                LegOutcome::passed(format!("`{mode}` search for `{query}` returned candidates"))
            }
            Some(_) => LegOutcome::failed(format!(
                "`{mode}` search for `{query}` returned zero candidates"
            )),
            None => LegOutcome::failed(format!(
                "`{mode}` search returned an error: {}",
                error_detail(response)
            )),
        },
        Err(error) => LegOutcome::failed(format!("`{mode}` search could not be served: {error}")),
    }
}

/// PURE: the NEGATIVE missing-id leg PROVES not-found against the site CONTRACT — it does not accept "any
/// error". It PASSES only on the two not-found shapes the contract emits: an EMPTY `documents` array, or a
/// served [`ErrorCode::NoResults`]. It FAILS a non-empty `documents` response (whether it leaks the absent
/// id OR returns unrelated documents — there is no first-class not-found signal in that shape), FAILS a
/// served error of any OTHER code (`Internal`/`DependencyUnavailable`/etc. are faults, not not-found), and
/// FAILS a transport fault (a dropped connection is not a valid negative result).
#[must_use]
pub fn classify_negative_missing(outcome: &Outcome, missing_id: &str) -> LegOutcome {
    match outcome {
        Ok(SessionResponse::Ok { result, .. }) => {
            if fetch_returned_id(result, missing_id) {
                LegOutcome::failed(format!(
                    "negative leg FAILED: `fetch {missing_id}` returned the supposedly-absent document"
                ))
            } else if documents_is_empty_array(result) {
                LegOutcome::passed(format!(
                    "missing id `{missing_id}` correctly returned an empty `documents` array"
                ))
            } else {
                LegOutcome::failed(format!(
                    "negative leg FAILED: `fetch {missing_id}` did not return the contract's not-found \
                     shape — `documents` must EXIST and be an empty array (a missing/non-array \
                     `documents`, a non-empty array of unrelated documents, or any other success body \
                     is not a not-found signal; expected an empty `documents` array or a served \
                     NoResults error)"
                ))
            }
        }
        Ok(SessionResponse::Err { error, .. }) => match error.code {
            ErrorCode::NoResults => LegOutcome::passed(format!(
                "missing id `{missing_id}` correctly returned a served NoResults not-found error"
            )),
            other => LegOutcome::failed(format!(
                "negative leg FAILED: `fetch {missing_id}` returned `{other:?}` ({}), not a not-found \
                 (empty `documents` or NoResults)",
                error.message
            )),
        },
        Err(error) => LegOutcome::failed(format!(
            "negative leg could not be served (a transport fault is not a valid not-found): {error}"
        )),
    }
}

/// PURE: the NEGATIVE bad-query leg asserts the CONTRACT for a malformed (empty) query — not "any error".
/// It PASSES on a served [`ErrorCode::BadInput`] (the contract's malformed-input signal) OR on a clean
/// EMPTY result (no candidates — the explicitly-permitted empty shape). It FAILS a served error of any
/// OTHER code (`Internal`/`DependencyUnavailable`/etc. mean the site mishandled the input rather than
/// rejecting it cleanly), FAILS an `Ok` that returns candidates for an empty query (the bad input was not
/// handled as bad), and FAILS a transport fault (a crash / dropped connection).
#[must_use]
pub fn classify_negative_bad_query(outcome: &Outcome) -> LegOutcome {
    match outcome {
        Ok(SessionResponse::Err { error, .. }) => match error.code {
            ErrorCode::BadInput => LegOutcome::passed(
                "malformed query was rejected with a served BadInput error (handled)",
            ),
            other => LegOutcome::failed(format!(
                "negative leg FAILED: malformed query returned `{other:?}` ({}), not a clean BadInput \
                 rejection or empty result",
                error.message
            )),
        },
        Ok(SessionResponse::Ok { result, .. }) => {
            if candidates_is_empty_array(result) {
                LegOutcome::passed(
                    "malformed query was handled with a clean empty result (no candidates)",
                )
            } else if search_has_candidates(result) {
                LegOutcome::failed(
                    "negative leg FAILED: a malformed (empty) query unexpectedly returned candidates — \
                     the bad input was neither rejected (BadInput) nor handled as an empty result",
                )
            } else {
                LegOutcome::failed(
                    "negative leg FAILED: a malformed (empty) query returned a non-contract success body \
                     — the clean-empty allowance requires `candidates` to EXIST and be an empty array (a \
                     missing or non-array `candidates` is not a valid empty result; expected an empty \
                     `candidates` array or a served BadInput error)",
                )
            }
        }
        Err(error) => LegOutcome::failed(format!(
            "malformed query was NOT handled — the request could not be served: {error}"
        )),
    }
}

fn error_detail(response: &SessionResponse) -> String {
    response.error().map_or_else(
        || "an error response".to_owned(),
        |error| error.message.clone(),
    )
}

// ---- the live runner: dial the endpoint, feed real responses into the pure classifiers ------------

/// Run every smoke leg against `endpoint`, returning the full report. Uses the thin `jurisearch-client`
/// to send REAL requests; each response (or transport error) is classified by the SAME pure functions
/// the unit tests cover, so the operated outcome and the tested outcome cannot diverge.
#[must_use]
pub fn run_smoke(endpoint: &SiteEndpoint, plan: &SmokePlan) -> SmokeReport {
    let mut report = SmokeReport::default();

    let status = send_request(endpoint, &status_probe_request());
    report.push(SmokeLeg::Status, classify_status(&status));

    let fetch = send_request(endpoint, &fetch_request(&plan.known_id));
    report.push(
        SmokeLeg::FetchKnownId,
        classify_fetch_known(&fetch, &plan.known_id),
    );

    let bm25 = send_request(endpoint, &search_request(&plan.query_term, "bm25"));
    report.push(
        SmokeLeg::Bm25Search,
        classify_search(&bm25, "bm25", &plan.query_term),
    );

    if plan.hybrid_enabled {
        let hybrid = send_request(endpoint, &search_request(&plan.query_term, "hybrid"));
        report.push(
            SmokeLeg::HybridSearch,
            classify_search(&hybrid, "hybrid", &plan.query_term),
        );
    } else {
        report.push(
            SmokeLeg::HybridSearch,
            LegOutcome::skipped(plan.hybrid_skip_reason.clone()),
        );
    }

    let missing = send_request(endpoint, &fetch_request(&plan.missing_id));
    report.push(
        SmokeLeg::NegativeMissingId,
        classify_negative_missing(&missing, &plan.missing_id),
    );

    let bad = send_request(endpoint, &bad_query_request());
    report.push(
        SmokeLeg::NegativeBadQuery,
        classify_negative_bad_query(&bad),
    );

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use jurisearch_core::error::ErrorObject;

    fn ok(result: Value) -> Outcome {
        Ok(SessionResponse::ok(None, result))
    }
    fn err_response(message: &str) -> Outcome {
        Ok(SessionResponse::err(None, ErrorObject::internal(message)))
    }
    fn err_code(code: ErrorCode, message: &str) -> Outcome {
        Ok(SessionResponse::err(
            None,
            ErrorObject {
                code,
                message: message.to_owned(),
                suggestions: Vec::new(),
            },
        ))
    }
    fn unreachable() -> Outcome {
        Err(ClientError::Unreachable {
            endpoint: "tcp://h:1".to_owned(),
            source: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"),
        })
    }

    #[test]
    fn status_passes_on_ok_and_fails_on_error_or_unreachable() {
        assert!(matches!(
            classify_status(&ok(json!({ "service": "jurisearch-site" }))),
            LegOutcome::Passed { .. }
        ));
        assert!(classify_status(&err_response("snapshot unavailable")).is_failed());
        assert!(classify_status(&unreachable()).is_failed());
    }

    #[test]
    fn fetch_known_id_passes_only_when_the_document_comes_back() {
        let hit = ok(json!({ "documents": [ { "document_id": "FIX-1" } ] }));
        assert!(matches!(
            classify_fetch_known(&hit, "FIX-1"),
            LegOutcome::Passed { .. }
        ));
        let empty = ok(json!({ "documents": [] }));
        assert!(classify_fetch_known(&empty, "FIX-1").is_failed());
        assert!(classify_fetch_known(&err_response("boom"), "FIX-1").is_failed());
        assert!(classify_fetch_known(&unreachable(), "FIX-1").is_failed());
    }

    #[test]
    fn bm25_and_hybrid_search_pass_only_with_candidates() {
        let hit = ok(json!({ "candidates": [ { "document_id": "FIX-1" } ] }));
        assert!(matches!(
            classify_search(&hit, "bm25", "q"),
            LegOutcome::Passed { .. }
        ));
        let empty = ok(json!({ "candidates": [] }));
        assert!(classify_search(&empty, "hybrid", "q").is_failed());
        assert!(classify_search(&err_response("no embedder"), "hybrid", "q").is_failed());
    }

    #[test]
    fn negative_missing_id_asserts_the_not_found_contract_not_any_error() {
        // Empty `documents` array → the contract's not-found shape → pass.
        let empty = ok(json!({ "documents": [] }));
        assert!(matches!(
            classify_negative_missing(&empty, "ABSENT"),
            LegOutcome::Passed { .. }
        ));
        // A served NoResults code → the contract's not-found error → pass.
        assert!(matches!(
            classify_negative_missing(&err_code(ErrorCode::NoResults, "no results"), "ABSENT"),
            LegOutcome::Passed { .. }
        ));
        // The absent doc actually came back → FAIL (a false positive).
        let leaked = ok(json!({ "documents": [ { "document_id": "ABSENT" } ] }));
        assert!(classify_negative_missing(&leaked, "ABSENT").is_failed());
        // CONTRACT: unrelated documents are NOT a not-found signal → FAIL (was a false-green before).
        let unrelated = ok(json!({ "documents": [ { "document_id": "SOMETHING-ELSE" } ] }));
        assert!(classify_negative_missing(&unrelated, "ABSENT").is_failed());
        // CONTRACT: an Internal error is a fault, not a not-found → FAIL (was a false-green before).
        assert!(classify_negative_missing(&err_response("boom"), "ABSENT").is_failed());
        // A DependencyUnavailable error is likewise not a not-found → FAIL.
        assert!(
            classify_negative_missing(
                &err_code(ErrorCode::DependencyUnavailable, "pg down"),
                "ABSENT"
            )
            .is_failed()
        );
        // A transport fault is not a valid not-found → FAIL.
        assert!(classify_negative_missing(&unreachable(), "ABSENT").is_failed());
        // CONTRACT: a non-contract success body `{}` (no `documents` field) is NOT a not-found → FAIL.
        assert!(classify_negative_missing(&ok(json!({})), "ABSENT").is_failed());
        // CONTRACT: a wrong successful shape (`candidates` instead of `documents`) → FAIL.
        assert!(classify_negative_missing(&ok(json!({ "candidates": [] })), "ABSENT").is_failed());
        // CONTRACT: a NON-ARRAY `documents` field is not the empty-array not-found shape → FAIL.
        assert!(classify_negative_missing(&ok(json!({ "documents": {} })), "ABSENT").is_failed());
        assert!(classify_negative_missing(&ok(json!({ "documents": null })), "ABSENT").is_failed());
    }

    #[test]
    fn negative_bad_query_requires_bad_input_or_clean_empty_not_any_error() {
        // A served BadInput code → the contract's malformed-input rejection → pass.
        assert!(matches!(
            classify_negative_bad_query(&err_code(ErrorCode::BadInput, "empty query")),
            LegOutcome::Passed { .. }
        ));
        // A clean empty result (no candidates) → the explicitly-permitted empty shape → pass.
        assert!(matches!(
            classify_negative_bad_query(&ok(json!({ "candidates": [] }))),
            LegOutcome::Passed { .. }
        ));
        // CONTRACT: an Internal error means the bad input was mishandled, not cleanly rejected → FAIL.
        assert!(classify_negative_bad_query(&err_response("kaboom")).is_failed());
        // CONTRACT: a DependencyUnavailable error is unrelated to bad-input handling → FAIL.
        assert!(
            classify_negative_bad_query(&err_code(ErrorCode::DependencyUnavailable, "pg down"))
                .is_failed()
        );
        // CONTRACT: an empty query that returns candidates was not handled as bad input → FAIL.
        assert!(
            classify_negative_bad_query(&ok(json!({ "candidates": [ { "document_id": "X" } ] })))
                .is_failed()
        );
        // A transport fault (crash / dropped connection) → FAIL.
        assert!(classify_negative_bad_query(&unreachable()).is_failed());
        // CONTRACT: a non-contract success body `{}` (no `candidates` field) is not a clean-empty → FAIL.
        assert!(classify_negative_bad_query(&ok(json!({}))).is_failed());
        // CONTRACT: a wrong successful shape (`documents` instead of `candidates`) → FAIL.
        assert!(classify_negative_bad_query(&ok(json!({ "documents": [] }))).is_failed());
        // CONTRACT: a NON-ARRAY `candidates` field is not the empty-array clean-empty shape → FAIL.
        assert!(classify_negative_bad_query(&ok(json!({ "candidates": {} }))).is_failed());
        assert!(classify_negative_bad_query(&ok(json!({ "candidates": null }))).is_failed());
    }

    #[test]
    fn hybrid_skip_is_explicit_and_never_silent() {
        // A plan WITHOUT hybrid records the hybrid leg as a skip carrying a recorded reason.
        let plan = SmokePlan::without_hybrid(
            "FIX-1",
            "responsabilite",
            "ABSENT",
            "no loopback query embedder configured (model/tokenizer assets absent)",
        );
        let mut report = SmokeReport::default();
        // Simulate just the hybrid leg's branch (the live runner does the same with no endpoint dial).
        report.push(
            SmokeLeg::HybridSearch,
            LegOutcome::skipped(plan.hybrid_skip_reason.clone()),
        );
        assert!(
            report.invariant_no_silent_skip(),
            "a skip must carry a reason"
        );
        assert!(report.passed(), "a recorded skip does not fail the smoke");
    }

    #[test]
    fn the_report_is_red_when_any_leg_fails_and_a_skip_alone_stays_green() {
        let mut report = SmokeReport::default();
        report.push(SmokeLeg::Status, LegOutcome::passed("ok"));
        report.push(
            SmokeLeg::HybridSearch,
            LegOutcome::skipped("no embedder assets"),
        );
        assert!(report.passed());
        assert_eq!(report.exit_code(), 0);
        report.push(SmokeLeg::Bm25Search, LegOutcome::failed("zero candidates"));
        assert!(!report.passed());
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn every_outcome_in_a_built_report_has_an_explicit_reason() {
        // Build a full report through the classifiers and assert the structural no-silent-skip invariant.
        let mut report = SmokeReport::default();
        report.push(
            SmokeLeg::Status,
            classify_status(&ok(json!({ "service": "jurisearch-site" }))),
        );
        report.push(
            SmokeLeg::FetchKnownId,
            classify_fetch_known(
                &ok(json!({ "documents": [ { "document_id": "FIX-1" } ] })),
                "FIX-1",
            ),
        );
        report.push(
            SmokeLeg::Bm25Search,
            classify_search(&ok(json!({ "candidates": [ {} ] })), "bm25", "q"),
        );
        report.push(SmokeLeg::HybridSearch, LegOutcome::skipped("assets absent"));
        report.push(
            SmokeLeg::NegativeMissingId,
            classify_negative_missing(&ok(json!({ "documents": [] })), "ABSENT"),
        );
        report.push(
            SmokeLeg::NegativeBadQuery,
            classify_negative_bad_query(&err_code(ErrorCode::BadInput, "empty query")),
        );
        assert_eq!(report.legs.len(), 6);
        assert!(report.invariant_no_silent_skip());
        assert!(report.passed());
    }

    #[test]
    fn the_json_view_carries_passed_and_per_leg_outcome_tags() {
        let mut report = SmokeReport::default();
        report.push(SmokeLeg::Status, LegOutcome::passed("ok"));
        report.push(SmokeLeg::HybridSearch, LegOutcome::skipped("assets absent"));
        let json = report.to_json();
        assert_eq!(json["passed"], Value::Bool(true));
        assert_eq!(json["legs"][0]["code"], "smoke.status");
        assert_eq!(json["legs"][0]["outcome"], "passed");
        assert_eq!(json["legs"][1]["outcome"], "skipped");
        assert_eq!(json["legs"][1]["reason"], "assets absent");
    }
}
