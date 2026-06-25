//! Shared command request structs: the single deserialization + validation target for each
//! session-callable command, used by BOTH the one-shot clap path (built from the command's clap
//! `*Args` plus the global `--index-dir` via `*Args::into_request`) and the JSONL session path
//! (deserialized directly from the request JSON). Each request carries the command's fields plus
//! `index_dir` (server-injected into session JSON; supplied from the global CLI arg one-shot), so
//! the payload builders read `req.index_dir` internally instead of taking it as a separate arg.
//!
//! Index-dir-only commands (status/doctor/stats/model fetch) keep their leaf payload signatures and
//! their small session DTOs — they have no field duplication to consolidate here.

use std::path::PathBuf;

use crate::*;

/// `search` request: the whole-decision/zone retrieval input. Serde defaults MUST mirror the clap
/// `SearchArgs` defaults so the session path matches the one-shot path field-for-field.
#[derive(Debug, Deserialize)]
pub(crate) struct SearchRequest {
    pub(crate) query: String,
    #[serde(default = "default_cli_kind")]
    pub(crate) kind: CliKind,
    #[serde(default = "default_search_mode")]
    pub(crate) mode: CliSearchMode,
    #[serde(default = "default_output_format")]
    pub(crate) format: CliOutputFormat,
    #[serde(default = "default_group_by")]
    pub(crate) group_by: CliGroupBy,
    #[serde(default = "default_top_k")]
    pub(crate) top_k: u32,
    #[serde(default)]
    pub(crate) cursor: Option<String>,
    #[serde(default)]
    pub(crate) as_of: Option<String>,
    #[serde(default)]
    pub(crate) rrf_lexical_weight: Option<f64>,
    #[serde(default)]
    pub(crate) rrf_dense_weight: Option<f64>,
    #[serde(default)]
    pub(crate) probes: Option<u32>,
    #[serde(default)]
    pub(crate) court: Option<String>,
    #[serde(default)]
    pub(crate) formation: Option<String>,
    #[serde(default)]
    pub(crate) publication: Option<String>,
    #[serde(default)]
    pub(crate) decided_from: Option<String>,
    #[serde(default)]
    pub(crate) decided_to: Option<String>,
    #[serde(default)]
    pub(crate) zone: Option<CliZone>,
    #[serde(default)]
    pub(crate) authority_weight: Option<f64>,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

impl SearchRequest {
    pub(crate) fn retrieval_options(&self) -> RetrievalOptions {
        RetrievalOptions {
            rrf_lexical_weight: self.rrf_lexical_weight,
            rrf_dense_weight: self.rrf_dense_weight,
            ivfflat_probes: self.probes,
            authority_weight: self.authority_weight,
        }
    }

    pub(crate) fn decision_filters(&self) -> DecisionFilters<'_> {
        DecisionFilters {
            jurisdiction: self.court.as_deref(),
            formation: self.formation.as_deref(),
            publication: self.publication.as_deref(),
            decided_from: self.decided_from.as_deref(),
            decided_to: self.decided_to.as_deref(),
        }
    }
}

impl SearchArgs {
    /// Build the shared request from parsed clap args plus the global `--index-dir`. This is the one
    /// place the clap surface is mapped onto the request surface (the one-shot half of the two ways a
    /// request is produced; the session half is `serde_json::from_value::<SearchRequest>`).
    pub(crate) fn into_request(self, index_dir: Option<PathBuf>) -> SearchRequest {
        SearchRequest {
            query: self.query,
            kind: self.kind,
            mode: self.mode,
            format: self.format,
            group_by: self.group_by,
            top_k: self.top_k,
            cursor: self.cursor,
            as_of: self.as_of,
            rrf_lexical_weight: self.rrf_lexical_weight,
            rrf_dense_weight: self.rrf_dense_weight,
            probes: self.probes,
            court: self.court,
            formation: self.formation,
            publication: self.publication,
            decided_from: self.decided_from,
            decided_to: self.decided_to,
            zone: self.zone,
            authority_weight: self.authority_weight,
            index_dir,
        }
    }
}

/// `fetch` request: version-pinned stable IDs plus the optional decision-part overlay.
#[derive(Debug, Deserialize)]
pub(crate) struct FetchRequest {
    pub(crate) ids: Vec<String>,
    #[serde(default)]
    pub(crate) part: Option<String>,
    #[serde(default)]
    pub(crate) online: bool,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

impl FetchArgs {
    pub(crate) fn into_request(self, index_dir: Option<PathBuf>) -> FetchRequest {
        FetchRequest {
            ids: self.ids,
            part: self.part,
            online: self.online,
            index_dir,
        }
    }
}

/// `cite` request: citation/identifier to verify, with optional online corroboration.
#[derive(Debug, Deserialize)]
pub(crate) struct CiteRequest {
    pub(crate) cite: String,
    #[serde(default)]
    pub(crate) strict: bool,
    #[serde(default)]
    pub(crate) online: bool,
    #[serde(default)]
    pub(crate) as_of: Option<String>,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

impl CiteArgs {
    pub(crate) fn into_request(self, index_dir: Option<PathBuf>) -> CiteRequest {
        CiteRequest {
            cite: self.cite,
            strict: self.strict,
            online: self.online,
            as_of: self.as_of,
            index_dir,
        }
    }
}

/// `context` request: structural neighbourhood (ancestry, siblings) for a document.
#[derive(Debug, Deserialize)]
pub(crate) struct ContextRequest {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) siblings: bool,
    #[serde(default)]
    pub(crate) as_of: Option<String>,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

impl ContextArgs {
    pub(crate) fn into_request(self, index_dir: Option<PathBuf>) -> ContextRequest {
        ContextRequest {
            id: self.id,
            siblings: self.siblings,
            as_of: self.as_of,
            index_dir,
        }
    }
}

/// `related` request: depth-1 graph neighbours with authority signals.
#[derive(Debug, Deserialize)]
pub(crate) struct RelatedRequest {
    pub(crate) id: String,
    #[serde(default = "default_related_rel")]
    pub(crate) rel: String,
    #[serde(default = "default_related_limit")]
    pub(crate) limit: u32,
    #[serde(default = "default_related_depth")]
    pub(crate) depth: u32,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

impl RelatedArgs {
    pub(crate) fn into_request(self, index_dir: Option<PathBuf>) -> RelatedRequest {
        RelatedRequest {
            id: self.id,
            rel: self.rel,
            limit: self.limit,
            depth: self.depth,
            index_dir,
        }
    }
}

/// `compare` request: aligned bm25/dense/hybrid retriever comparison for one query.
#[derive(Debug, Deserialize)]
pub(crate) struct CompareRequest {
    pub(crate) query: String,
    #[serde(default = "default_compare_kind")]
    pub(crate) kind: CliKind,
    #[serde(default = "default_top_k")]
    pub(crate) top_k: u32,
    #[serde(default)]
    pub(crate) as_of: Option<String>,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

impl CompareArgs {
    pub(crate) fn into_request(self, index_dir: Option<PathBuf>) -> CompareRequest {
        CompareRequest {
            query: self.query,
            kind: self.kind,
            top_k: self.top_k,
            as_of: self.as_of,
            index_dir,
        }
    }
}

/// `inspect` request: the raw canonical record for one document id.
#[derive(Debug, Deserialize)]
pub(crate) struct InspectRequest {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

impl InspectArgs {
    pub(crate) fn into_request(self, index_dir: Option<PathBuf>) -> InspectRequest {
        InspectRequest {
            id: self.id,
            index_dir,
        }
    }
}

/// `versions` request: an article's full version-family timeline.
#[derive(Debug, Deserialize)]
pub(crate) struct VersionsRequest {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

impl VersionsArgs {
    pub(crate) fn into_request(self, index_dir: Option<PathBuf>) -> VersionsRequest {
        VersionsRequest {
            id: self.id,
            index_dir,
        }
    }
}

/// `diff` request: which article version was in force on two dates, and whether it changed.
#[derive(Debug, Deserialize)]
pub(crate) struct DiffRequest {
    pub(crate) id: String,
    pub(crate) from: String,
    pub(crate) to: String,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

impl DiffArgs {
    pub(crate) fn into_request(self, index_dir: Option<PathBuf>) -> DiffRequest {
        DiffRequest {
            id: self.id,
            from: self.from,
            to: self.to,
            index_dir,
        }
    }
}

/// `eval phase1` request: run or list the Phase-1 LEGI statutory-search fixtures.
#[derive(Debug, Deserialize)]
pub(crate) struct EvalPhase1Request {
    #[serde(default)]
    pub(crate) list: bool,
    #[serde(default)]
    pub(crate) include_dev: bool,
    #[serde(default = "default_search_mode")]
    pub(crate) mode: CliSearchMode,
    #[serde(default = "default_top_k")]
    pub(crate) top_k: u32,
    #[serde(default)]
    pub(crate) index_dir: Option<PathBuf>,
}

impl EvalPhase1Args {
    pub(crate) fn into_request(self, index_dir: Option<PathBuf>) -> EvalPhase1Request {
        EvalPhase1Request {
            list: self.list,
            include_dev: self.include_dev,
            mode: self.mode,
            top_k: self.top_k,
            index_dir,
        }
    }
}
