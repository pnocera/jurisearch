//! The **site query operation vocabulary** and the one `command` ⇄ `Operation` mapping
//! (contract-owned).
//!
//! `Operation` is the deliberately **narrow** client-facing query surface — search / fetch / cite /
//! related / context / compare / status. It is NOT the full local command inventory in
//! [`crate::contract::COMMANDS`] (that table also lists local/admin/session/one-shot surfaces). The
//! site dispatcher (work/09 P4) registers handlers for **only** these operations; every other
//! command string — local-only (`expand`, `model fetch`, `eval …`, `setup`, `doctor`, `stats`,
//! `inspect`, `versions`, `diff`, `help`, `schema`, `exit`) and session-excluded one-shots
//! (`ingest`, `sync`, …) — is rejected here with a session [`ErrorObject`], never routed to the
//! local dispatcher / an `index_dir`-aware payload, and never a package `Reject`.
//!
//! Source of truth for the disposition is the "Session-surface compatibility matrix" in
//! `work/09-jurisearch-cli/04-implementation-plan.md`.

use crate::error::ErrorObject;

/// The closed set of client-facing query operations exposed by the site service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    Search,
    Fetch,
    Cite,
    Related,
    Context,
    Compare,
    Status,
}

impl Operation {
    /// Every site operation, in a stable order — the allowlist the site dispatcher registers.
    pub const ALL: [Operation; 7] = [
        Operation::Search,
        Operation::Fetch,
        Operation::Cite,
        Operation::Related,
        Operation::Context,
        Operation::Compare,
        Operation::Status,
    ];

    /// The canonical wire command string for this operation.
    pub fn as_command(self) -> &'static str {
        match self {
            Operation::Search => "search",
            Operation::Fetch => "fetch",
            Operation::Cite => "cite",
            Operation::Related => "related",
            Operation::Context => "context",
            Operation::Compare => "compare",
            Operation::Status => "status",
        }
    }

    /// Map a wire `command` string to its [`Operation`]. Anything outside the closed site set —
    /// unknown, legacy, local-only, admin, model, ingest, eval, or a session-control command — is a
    /// **session** error ([`ErrorObject`]), NOT a package `Reject`. The site dispatcher returns this
    /// as a `SessionResponse::Err`. Whitespace is trimmed (parity with the local dispatcher).
    pub fn parse_command(command: &str) -> Result<Self, ErrorObject> {
        match command.trim() {
            "search" => Ok(Operation::Search),
            "fetch" => Ok(Operation::Fetch),
            "cite" => Ok(Operation::Cite),
            "related" => Ok(Operation::Related),
            "context" => Ok(Operation::Context),
            "compare" => Ok(Operation::Compare),
            "status" => Ok(Operation::Status),
            other => Err(ErrorObject::bad_input(format!(
                "`{other}` is not a site query operation"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_operation() {
        for op in Operation::ALL {
            assert_eq!(
                Operation::parse_command(op.as_command()),
                Ok(op),
                "{} should round-trip",
                op.as_command()
            );
        }
    }

    #[test]
    fn parse_command_trims_whitespace() {
        assert_eq!(Operation::parse_command("  search "), Ok(Operation::Search));
    }

    /// Table-driven from the compatibility matrix: every command that is NOT a site operation must be
    /// rejected with a session error — exact session command strings, not prose labels.
    #[test]
    fn rejects_every_non_site_command() {
        let non_site = [
            // local-only query helper / management / diagnostics / control
            "expand",
            "model fetch",
            "eval phase1",
            "eval france-legi",
            "eval run",
            "eval tune",
            "setup",
            "doctor",
            "stats",
            "inspect",
            "versions",
            "diff",
            "help",
            "help agent",
            "help schema",
            "schema",
            "exit",
            // session-excluded one-shots
            "ingest",
            "sync",
            "serve",
            // genuinely unknown
            "",
            "definitely-not-a-command",
        ];
        for command in non_site {
            let result = Operation::parse_command(command);
            assert!(
                result.is_err(),
                "`{command}` must NOT be a site operation, got {result:?}"
            );
        }
    }
}
