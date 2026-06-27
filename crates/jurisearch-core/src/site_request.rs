//! The dependency-light SITE request DTOs + [`Operation::parse_args`] (work/09 — contract-owned request
//! seam). The site query service AND a future friendly thin client share these typed request shapes, so
//! request DEFAULTS and field VALIDATION have ONE authority in the contract crate — NOT `jurisearch-cli`
//! internals (which would let a client drift from server-side validation).
//!
//! These carry NONE of the server-owned data-source fields (`index_dir`) and NONE of the local-only
//! options that the site surface does not serve (search `zone`, `cite`/`fetch` `online`) — they are not
//! part of the site contract. Strict (`deny_unknown_fields`): an unsupported option is REJECTED, never
//! silently dropped, so a caller is never told an option was honored when the site ignored it.

use serde::Deserialize;
use serde_json::Value;

use crate::contract::{LegalKind, OutputFormat};
use crate::error::ErrorObject;
use crate::operation::Operation;
use crate::retrieval::{GroupBy, RetrievalMode, RetrievalOptions};

fn default_search_kind() -> LegalKind {
    LegalKind::All
}
fn default_search_mode() -> RetrievalMode {
    RetrievalMode::Hybrid
}
fn default_output_format() -> OutputFormat {
    OutputFormat::Concise
}
fn default_group_by() -> GroupBy {
    GroupBy::Chunk
}
fn default_top_k() -> u32 {
    10
}
fn default_compare_kind() -> LegalKind {
    LegalKind::Code
}
fn default_related_rel() -> String {
    "cites".to_owned()
}
fn default_related_limit() -> u32 {
    50
}
fn default_related_depth() -> u32 {
    1
}

/// `search`: hybrid/bm25/dense retrieval over the site corpora. No `zone` (a Cassation-only client/online
/// concern) and no `index_dir` (server-owned).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteSearchArgs {
    pub query: String,
    #[serde(default = "default_search_kind")]
    pub kind: LegalKind,
    #[serde(default = "default_search_mode")]
    pub mode: RetrievalMode,
    #[serde(default = "default_output_format")]
    pub format: OutputFormat,
    #[serde(default = "default_group_by")]
    pub group_by: GroupBy,
    #[serde(default = "default_top_k")]
    pub top_k: u32,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub as_of: Option<String>,
    #[serde(default)]
    pub rrf_lexical_weight: Option<f64>,
    #[serde(default)]
    pub rrf_dense_weight: Option<f64>,
    #[serde(default)]
    pub probes: Option<u32>,
    #[serde(default)]
    pub court: Option<String>,
    #[serde(default)]
    pub formation: Option<String>,
    #[serde(default)]
    pub publication: Option<String>,
    #[serde(default)]
    pub decided_from: Option<String>,
    #[serde(default)]
    pub decided_to: Option<String>,
    #[serde(default)]
    pub authority_weight: Option<f64>,
}

impl SiteSearchArgs {
    /// The retrieval-tuning overrides as the contract [`RetrievalOptions`] shape.
    #[must_use]
    pub fn retrieval_options(&self) -> RetrievalOptions {
        RetrievalOptions {
            rrf_lexical_weight: self.rrf_lexical_weight,
            rrf_dense_weight: self.rrf_dense_weight,
            ivfflat_probes: self.probes,
            authority_weight: self.authority_weight,
        }
    }
}

/// `fetch`: exact, version-pinned document fetch. Base fetch only — no `part`/`online` overlay (a
/// client/online concern), no `index_dir`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteFetchArgs {
    pub ids: Vec<String>,
}

/// `cite`: citation-state classification. No `online` (the Légifrance probe is a client/online concern).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteCiteArgs {
    pub cite: String,
    #[serde(default)]
    pub strict: bool,
    #[serde(default)]
    pub as_of: Option<String>,
}

/// `context`: structural ancestry/siblings for one document.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteContextArgs {
    pub id: String,
    #[serde(default)]
    pub siblings: bool,
    #[serde(default)]
    pub as_of: Option<String>,
}

/// `related`: depth-1 graph neighbours with authority signals.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteRelatedArgs {
    pub id: String,
    #[serde(default = "default_related_rel")]
    pub rel: String,
    #[serde(default = "default_related_limit")]
    pub limit: u32,
    #[serde(default = "default_related_depth")]
    pub depth: u32,
}

/// `compare`: aligned bm25/dense/hybrid retriever comparison for one query.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteCompareArgs {
    pub query: String,
    #[serde(default = "default_compare_kind")]
    pub kind: LegalKind,
    #[serde(default = "default_top_k")]
    pub top_k: u32,
    #[serde(default)]
    pub as_of: Option<String>,
}

/// `status`: the site health report. Carries NO arguments — strict (`deny_unknown_fields`) so an
/// unsupported field is REJECTED at the same seam as every other operation, never silently accepted.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiteStatusArgs {}

/// A parsed, typed site request — the result of [`Operation::parse_args`].
#[derive(Debug, Clone)]
pub enum SiteRequest {
    Search(SiteSearchArgs),
    Fetch(SiteFetchArgs),
    Cite(SiteCiteArgs),
    Related(SiteRelatedArgs),
    Context(SiteContextArgs),
    Compare(SiteCompareArgs),
    /// `status` carries no arguments (its args are still strict-validated: a non-empty payload is
    /// rejected at the seam, like every other operation).
    Status,
}

impl Operation {
    /// Parse + validate the wire `args` for this operation into the typed [`SiteRequest`]. The single
    /// contract-owned boundary the site handlers (and a future thin client) use, so request defaults and
    /// field validation never diverge. A malformed payload or unsupported field is a `bad_input`
    /// [`ErrorObject`] (the same shaping the dispatcher wraps into a `SessionResponse::Err`).
    ///
    /// # Errors
    /// [`ErrorObject`] (`bad_input`) for malformed JSON args or an unsupported/unknown field.
    pub fn parse_args(self, args: &Value) -> Result<SiteRequest, ErrorObject> {
        fn parse<T: serde::de::DeserializeOwned>(
            operation: &str,
            args: &Value,
        ) -> Result<T, ErrorObject> {
            serde_json::from_value(args.clone()).map_err(|error| {
                ErrorObject::bad_input(format!("invalid {operation} args: {error}"))
            })
        }
        Ok(match self {
            Operation::Search => SiteRequest::Search(parse("search", args)?),
            Operation::Fetch => SiteRequest::Fetch(parse("fetch", args)?),
            Operation::Cite => SiteRequest::Cite(parse("cite", args)?),
            Operation::Related => SiteRequest::Related(parse("related", args)?),
            Operation::Context => SiteRequest::Context(parse("context", args)?),
            Operation::Compare => SiteRequest::Compare(parse("compare", args)?),
            Operation::Status => {
                // Status takes no args, but they are STILL strict-validated: a non-empty payload is an
                // unsupported field, rejected here instead of being silently accepted by the server.
                let _: SiteStatusArgs = parse("status", args)?;
                SiteRequest::Status
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn search_args_default_and_strict() {
        let parsed = Operation::Search
            .parse_args(&json!({ "query": "responsabilite" }))
            .expect("defaults apply");
        let SiteRequest::Search(args) = parsed else {
            panic!("expected search");
        };
        assert_eq!(args.kind, LegalKind::All);
        assert_eq!(args.mode, RetrievalMode::Hybrid);
        assert_eq!(args.group_by, GroupBy::Chunk);
        assert_eq!(args.top_k, 10);
        // An unsupported field (a server-owned `index_dir` or local-only `zone`) is REJECTED.
        assert!(
            Operation::Search
                .parse_args(&json!({ "query": "x", "index_dir": "/tmp" }))
                .is_err()
        );
        assert!(
            Operation::Search
                .parse_args(&json!({ "query": "x", "zone": "motivations" }))
                .is_err()
        );
    }

    #[test]
    fn cite_and_fetch_reject_online_and_index_dir() {
        assert!(
            Operation::Cite
                .parse_args(&json!({ "cite": "x", "online": true }))
                .is_err(),
            "online is not part of the site cite contract"
        );
        assert!(
            Operation::Fetch
                .parse_args(&json!({ "ids": ["x"], "part": "motivations" }))
                .is_err(),
            "the decision-part overlay is not part of the site fetch contract"
        );
        // `status` takes no args: an empty object is fine, a non-empty one is REJECTED at the seam (not
        // silently accepted), so the "one authority for validation" claim holds for the WHOLE surface.
        assert!(matches!(
            Operation::Status.parse_args(&json!({})).unwrap(),
            SiteRequest::Status
        ));
        assert!(
            Operation::Status
                .parse_args(&json!({ "bogus": true }))
                .is_err(),
            "a non-empty status payload must be rejected like every other operation"
        );
    }
}
