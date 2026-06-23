use serde::{Deserialize, Serialize};

use crate::SCHEMA_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    Concise,
    Detailed,
}

impl OutputFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Concise => "concise",
            Self::Detailed => "detailed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegalKind {
    Code,
    Decision,
    All,
}

impl LegalKind {
    pub fn canonical_result_kind(self) -> &'static str {
        match self {
            Self::Code => "article",
            Self::Decision => "decision",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationState {
    Exact,
    Normalized,
    Ambiguous,
    StaleVersion,
    NotFound,
    SourceUnavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Implemented,
    Stub,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub status: CommandStatus,
    pub request_schema: &'static str,
    pub response_schema: &'static str,
}

pub const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        name: "search",
        summary: "Return compact ranked candidates for a legal research query.",
        status: CommandStatus::Implemented,
        request_schema: "SearchRequest",
        response_schema: "SearchResponse",
    },
    CommandSpec {
        name: "compare",
        summary: "Compare bm25/dense/hybrid retrievers for one query: aligned top-k, pooled union, and overlap.",
        status: CommandStatus::Implemented,
        request_schema: "CompareRequest",
        response_schema: "CompareResponse",
    },
    CommandSpec {
        name: "fetch",
        summary: "Return full source text for selected stable IDs.",
        status: CommandStatus::Implemented,
        request_schema: "FetchRequest",
        response_schema: "FetchResponse",
    },
    CommandSpec {
        name: "cite",
        summary: "Verify citations and identifiers with explicit citation states.",
        status: CommandStatus::Implemented,
        request_schema: "CiteRequest",
        response_schema: "CiteResponse",
    },
    CommandSpec {
        name: "related",
        summary: "Return depth-1 graph neighbours (cites / cited_by / temporal) with authority signals.",
        status: CommandStatus::Implemented,
        request_schema: "RelatedRequest",
        response_schema: "RelatedResponse",
    },
    CommandSpec {
        name: "context",
        summary: "Return structural neighbourhood: ancestry, siblings, or decision zones.",
        status: CommandStatus::Implemented,
        request_schema: "ContextRequest",
        response_schema: "ContextResponse",
    },
    CommandSpec {
        name: "expand",
        summary: "Return curated legal-vocabulary expansions for a query.",
        status: CommandStatus::Implemented,
        request_schema: "ExpandRequest",
        response_schema: "ExpandResponse",
    },
    CommandSpec {
        name: "status",
        summary: "Report corpus coverage, freshness, model fingerprints, and index health.",
        status: CommandStatus::Implemented,
        request_schema: "StatusRequest",
        response_schema: "StatusResponse",
    },
    CommandSpec {
        name: "model fetch",
        summary: "Explicitly fetch local in-process models; never implicit during search.",
        status: CommandStatus::Implemented,
        request_schema: "ModelFetchRequest",
        response_schema: "ModelFetchResponse",
    },
    CommandSpec {
        name: "setup",
        summary: "Check or prepare local configuration and optional model caches.",
        status: CommandStatus::Implemented,
        request_schema: "SetupRequest",
        response_schema: "SetupResponse",
    },
    CommandSpec {
        name: "doctor",
        summary: "Non-owning dependency preflight (embedding, models, PG runtime, extensions, index dir); does not start Postgres.",
        status: CommandStatus::Implemented,
        request_schema: "DoctorRequest",
        response_schema: "DoctorResponse",
    },
    CommandSpec {
        name: "stats",
        summary: "Report corpus/graph/embedding counts (replaces ad-hoc psql for introspection).",
        status: CommandStatus::Implemented,
        request_schema: "StatsRequest",
        response_schema: "StatsResponse",
    },
    CommandSpec {
        name: "inspect",
        summary: "Return the raw canonical record for one document id (full row, chunk count, edge count).",
        status: CommandStatus::Implemented,
        request_schema: "InspectRequest",
        response_schema: "InspectResponse",
    },
    CommandSpec {
        name: "versions",
        summary: "List an article's version timeline (every member of its version family by validity start).",
        status: CommandStatus::Implemented,
        request_schema: "VersionsRequest",
        response_schema: "VersionsResponse",
    },
    CommandSpec {
        name: "diff",
        summary: "Compare the article versions in force on two dates (which version, and whether it changed).",
        status: CommandStatus::Implemented,
        request_schema: "DiffRequest",
        response_schema: "DiffResponse",
    },
    CommandSpec {
        name: "session --jsonl",
        summary: "Warm JSONL subprocess protocol for order-preserving agent workflows.",
        status: CommandStatus::Implemented,
        request_schema: "SessionRequest",
        response_schema: "SessionResponse",
    },
    CommandSpec {
        name: "batch --jsonl",
        summary: "Finite JSONL protocol for eval and bulk verification runs.",
        status: CommandStatus::Implemented,
        request_schema: "SessionRequest",
        response_schema: "SessionResponse",
    },
    CommandSpec {
        name: "ingest",
        summary: "Build or inspect official-source ingestion plans and canonical records.",
        status: CommandStatus::Implemented,
        request_schema: "IngestRequest",
        response_schema: "IngestResponse",
    },
    CommandSpec {
        name: "eval phase1",
        summary: "List or execute built-in Phase 1 LEGI retrieval evaluation fixtures.",
        status: CommandStatus::Implemented,
        request_schema: "EvalPhase1Request",
        response_schema: "EvalPhase1Response",
    },
    CommandSpec {
        name: "eval france-legi",
        summary: "Run the France-LEGI official-evidence benchmark and emit a phase1_france_legi_benchmark artifact (one-shot CLI only).",
        status: CommandStatus::Implemented,
        request_schema: "EvalFranceLegiRequest",
        response_schema: "EvalFranceLegiResponse",
    },
    CommandSpec {
        name: "eval run",
        summary: "Run a custom retrieval eval (your questions + qrels or external judge) and emit an eval_run artifact (one-shot CLI only).",
        status: CommandStatus::Implemented,
        request_schema: "EvalRunRequest",
        response_schema: "EvalRunResponse",
    },
    CommandSpec {
        name: "eval tune",
        summary: "Sweep a hybrid retrieval parameter (rrf weights / probes) against a fixture and report the metric-maximizing value (one-shot CLI only).",
        status: CommandStatus::Implemented,
        request_schema: "EvalTuneRequest",
        response_schema: "EvalTuneResponse",
    },
    CommandSpec {
        name: "sync",
        summary: "Synchronize official sources through deltas or transactional histories.",
        status: CommandStatus::Stub,
        request_schema: "SyncRequest",
        response_schema: "SyncResponse",
    },
    CommandSpec {
        name: "help agent",
        summary: "Print the compiled agent-facing contract.",
        status: CommandStatus::Implemented,
        request_schema: "HelpAgentRequest",
        response_schema: "HelpAgentResponse",
    },
    CommandSpec {
        name: "help schema --json",
        summary: "Print machine-readable schemas for command requests, responses, and errors.",
        status: CommandStatus::Implemented,
        request_schema: "HelpSchemaRequest",
        response_schema: "HelpSchemaResponse",
    },
];

/// `COMMANDS` names that are NOT callable over the warm session protocol — one-shot CLI only,
/// or stubs not yet implemented anywhere. Kept in sync with the CLI's `dispatch_session_request`
/// (the `not_implemented` arm) and enforced by tests. The `session --jsonl` / `batch --jsonl`
/// entries are the protocol itself and are intentionally not listed here.
pub const SESSION_EXCLUDED_COMMANDS: &[&str] =
    &["ingest", "eval france-legi", "eval run", "eval tune", "sync"];

/// True when `name` is callable over the warm session protocol.
pub fn command_session_available(name: &str) -> bool {
    !SESSION_EXCLUDED_COMMANDS.contains(&name)
        && name != "session --jsonl"
        && name != "batch --jsonl"
}

pub fn agent_help() -> String {
    let mut out = String::new();
    out.push_str("# jurisearch agent contract\n\n");
    out.push_str("schema_version: ");
    out.push_str(SCHEMA_VERSION);
    out.push_str("\n\n");
    out.push_str("jurisearch is a CLI-only French legal search engine for AI agents. ");
    out.push_str(
        "Machine-readable command output is JSON on stdout; diagnostics go to stderr.\n\n",
    );
    out.push_str("## Commands\n\n");
    for command in COMMANDS {
        out.push_str("- `");
        out.push_str(command.name);
        out.push_str("` — ");
        out.push_str(command.summary);
        if SESSION_EXCLUDED_COMMANDS.contains(&command.name) {
            out.push_str(" (one-shot CLI only — not available over the session protocol)");
        }
        out.push('\n');
    }
    out.push_str("\n## Exit codes\n\n");
    out.push_str("- `0`: success\n");
    out.push_str("- `2`: user input, no-results, strict citation, or validation failure\n");
    out.push_str("- `3`: local index/configuration unavailable\n");
    out.push_str("- `4`: local dependency or implementation unavailable\n");
    out.push_str("- `5`: upstream official API or provider failure\n\n");
    out.push_str("## Session JSONL\n\n");
    out.push_str("Request: `{ \"id\":\"req-1\", \"command\":\"search\", \"args\":{\"query\":\"article 1240\"} }`\n");
    out.push_str("Response: `{ \"id\":\"req-1\", \"ok\":true, \"result\":{...} }`\n");
    out.push_str(
        "Malformed input returns a JSONL error with `id:null` and does not kill the session.\n",
    );
    out
}
