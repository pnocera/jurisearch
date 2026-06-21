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
        name: "fetch",
        summary: "Return full source text for selected stable IDs.",
        status: CommandStatus::Implemented,
        request_schema: "FetchRequest",
        response_schema: "FetchResponse",
    },
    CommandSpec {
        name: "cite",
        summary: "Verify citations and identifiers with explicit citation states.",
        status: CommandStatus::Stub,
        request_schema: "CiteRequest",
        response_schema: "CiteResponse",
    },
    CommandSpec {
        name: "related",
        summary: "Return bounded graph neighbours with authority signals.",
        status: CommandStatus::Stub,
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
        status: CommandStatus::Stub,
        request_schema: "ModelFetchRequest",
        response_schema: "ModelFetchResponse",
    },
    CommandSpec {
        name: "setup",
        summary: "Check or prepare local configuration and optional model caches.",
        status: CommandStatus::Stub,
        request_schema: "SetupRequest",
        response_schema: "SetupResponse",
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
