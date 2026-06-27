//! work/09 P4 — the SITE dispatcher: the server-owned, allowlist-by-construction request router for the
//! query service. This is a NEW surface, disjoint from the local `serve.rs` session loop — a site request
//! NEVER reaches `dispatch_session_request` (the local, `index_dir`-injecting path).
//!
//! The security boundary, expressed as code: only the closed [`Operation`] set has handlers (everything
//! else is rejected), the server owns the [`QueryStore`] (the client's `index_dir`/data-source hints are
//! rejected before dispatch), and handlers return a bare result body — the dispatcher attaches the request
//! id and constructs the [`SessionResponse`], so correlation and error wrapping stay centralized.

use std::collections::HashMap;

use jurisearch_core::error::{ErrorCode, ErrorObject};
use jurisearch_core::operation::Operation;
use jurisearch_core::session::{SessionRequest, SessionResponse};
use jurisearch_storage::query::QueryStore;
use serde_json::Value;

/// Server-owned, long-lived dependencies injected into every handler — NEVER taken from the client. The
/// active corpus topology is resolved per request via `store.begin_snapshot()`, not pre-resolved here.
pub(crate) struct ServerContext<'a> {
    pub(crate) store: &'a dyn QueryStore,
}

/// One handler per exposed [`Operation`]. Returns the bare result body; the dispatcher wraps it (with the
/// request id) into a [`SessionResponse`]. A handler performs NO write and resolves NO `index_dir`.
pub(crate) trait OperationHandler: Send + Sync {
    fn handle(&self, ctx: &ServerContext, args: &Value) -> Result<Value, ErrorObject>;
}

/// The site request router. The handler map IS the allowlist: only registered [`Operation`]s are served,
/// and only the closed `Operation` set can be registered (a non-operation command never parses).
pub(crate) struct SiteDispatcher {
    handlers: HashMap<Operation, Box<dyn OperationHandler>>,
}

impl SiteDispatcher {
    pub(crate) fn new() -> Self {
        Self {
            handlers: HashMap::new(),
        }
    }

    /// Register the handler for `operation`. (Adding a brand-new operation is an explicit change to the
    /// closed `Operation` vocabulary; the dispatch loop itself is closed for modification — OCP.)
    pub(crate) fn register(
        &mut self,
        operation: Operation,
        handler: Box<dyn OperationHandler>,
    ) -> &mut Self {
        self.handlers.insert(operation, handler);
        self
    }

    /// Route one request: reject client-owned data-source fields → allowlist (parse + registration) →
    /// dispatch → wrap. A command outside the exposed set, or a valid-but-unregistered operation, returns
    /// an `ErrorObject` and never reaches the local dispatcher or an `index_dir`-aware payload.
    pub(crate) fn dispatch(
        &self,
        ctx: &ServerContext,
        request: &SessionRequest,
    ) -> SessionResponse {
        let id = request.id.clone();

        // 1. The server owns its data source — a client may not supply `index_dir` (or any other
        //    data-source hint). Reject BEFORE dispatch, so a filesystem hint can never influence the
        //    service (stronger than silently stripping it).
        if let Some(field) = client_data_source_field(&request.args) {
            return SessionResponse::err(
                id,
                ErrorObject::bad_input(format!(
                    "the site service owns its data source; the client field `{field}` may not be \
                     supplied to a site request"
                )),
            );
        }

        // 2. Allowlist: a non-operation command never parses; a valid-but-unregistered operation is
        //    `not_implemented`. Either way it never falls through to the local dispatcher.
        let operation = match Operation::parse_command(&request.command) {
            Ok(operation) => operation,
            Err(error) => return SessionResponse::err(id, error),
        };
        let Some(handler) = self.handlers.get(&operation) else {
            return SessionResponse::err(id, site_operation_not_registered(operation.as_command()));
        };

        // 3. Dispatch; the dispatcher owns id correlation + response wrapping.
        match handler.handle(ctx, &request.args) {
            Ok(result) => SessionResponse::ok(id, result),
            Err(error) => SessionResponse::err(id, error),
        }
    }
}

/// A valid site `Operation` that has no handler in the current service slice. Phase-agnostic (unlike the
/// core `ErrorObject::not_implemented`, which references a Phase 0 scaffold), with the `NotImplemented`
/// code so callers can distinguish "valid op, not served here" from a bad request.
fn site_operation_not_registered(command: &str) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::NotImplemented,
        message: format!(
            "`{command}` is a valid site operation but is not registered in this query-service slice"
        ),
        suggestions: vec![
            "This operation is served in a later work/09 slice of the site query service.".into(),
        ],
    }
}

/// The client-owned data-source fields a site request may NOT carry (the server owns its store). Returns
/// the offending field name if present.
fn client_data_source_field(args: &Value) -> Option<&'static str> {
    const FORBIDDEN: &[&str] = &["index_dir"];
    let object = args.as_object()?;
    FORBIDDEN
        .iter()
        .copied()
        .find(|field| object.contains_key(*field))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jurisearch_storage::query::ReadSnapshot;
    use jurisearch_storage::runtime::StorageError;
    use serde_json::json;

    /// A store whose snapshot must NEVER be opened: any handler reaching it (instead of being rejected by
    /// the allowlist / data-source boundary) panics the test.
    struct PanicStore;
    impl QueryStore for PanicStore {
        fn begin_snapshot(&self) -> Result<Box<dyn ReadSnapshot + '_>, StorageError> {
            panic!("the request should have been rejected before any handler opened a snapshot");
        }
    }

    fn skeleton() -> SiteDispatcher {
        // Mirror the 4A registration (fetch + status) without pulling the module's PG-touching handlers
        // into a unit test: register a handler that would panic if a snapshot were opened.
        struct PanicHandler;
        impl OperationHandler for PanicHandler {
            fn handle(&self, ctx: &ServerContext, _args: &Value) -> Result<Value, ErrorObject> {
                ctx.store
                    .begin_snapshot()
                    .map_err(|_| ErrorObject::bad_input("x"))?;
                unreachable!()
            }
        }
        let mut dispatcher = SiteDispatcher::new();
        dispatcher.register(Operation::Fetch, Box::new(PanicHandler));
        dispatcher
    }

    fn request(command: &str, args: Value) -> SessionRequest {
        SessionRequest {
            id: Some(json!(1)),
            command: command.to_owned(),
            args,
        }
    }

    #[test]
    fn every_non_site_command_is_rejected_by_the_allowlist() {
        let dispatcher = skeleton();
        let ctx = ServerContext { store: &PanicStore };
        // Table-driven from the §45 compatibility matrix: EVERY local-only / non-site command is rejected
        // (it is not an `Operation`, so it never parses) — never a fall-through to a local payload, never
        // a snapshot (the PanicStore proves it). Covers query helpers, model/eval/management, diagnostics,
        // control, and a representative session-excluded one-shot.
        let non_site = [
            "expand", "model", "eval", "setup", "doctor", "stats", "inspect", "versions", "diff",
            "help", "schema", "exit", "ingest",
        ];
        for command in non_site {
            let response = dispatcher.dispatch(&ctx, &request(command, json!({})));
            assert!(
                !response.is_ok(),
                "non-site command `{command}` must be rejected by the site allowlist"
            );
            assert_eq!(response.id(), Some(&json!(1)), "the error correlates by id");
        }
    }

    #[test]
    fn a_valid_but_unregistered_operation_is_not_implemented() {
        let dispatcher = skeleton();
        let ctx = ServerContext { store: &PanicStore };
        // `search` IS an exposed Operation but is unregistered in the 4A skeleton → a NotImplemented error
        // (not a generic bad_input), never a fall-through to a local payload.
        let response = dispatcher.dispatch(&ctx, &request("search", json!({"query": "x"})));
        assert_eq!(response.id(), Some(&json!(1)), "the error correlates by id");
        let error = response.error().expect("unregistered op errors");
        assert_eq!(
            error.code,
            ErrorCode::NotImplemented,
            "a valid-but-unregistered op is NotImplemented, not bad_input"
        );
        assert!(
            error.message.contains("search"),
            "the error names the unregistered command: {}",
            error.message
        );
    }

    #[test]
    fn a_client_index_dir_is_rejected_before_any_handler() {
        let dispatcher = skeleton();
        let ctx = ServerContext { store: &PanicStore };
        // A sentinel `index_dir` (valid for the LOCAL dispatcher) is rejected at the boundary — the
        // PanicStore proves the (registered) fetch handler never ran.
        let response = dispatcher.dispatch(
            &ctx,
            &request(
                "fetch",
                json!({"ids": ["cass:X"], "index_dir": "/tmp/evil"}),
            ),
        );
        let error = response.error().expect("index_dir is rejected");
        assert!(error.message.contains("index_dir"), "{}", error.message);
    }
}
