//! work/09 P4 — the site query service: a server-owned, read-only-safe, allowlist-by-construction
//! dispatcher over the versioned JSONL site transport. Disjoint from the local `serve.rs` session loop;
//! a site request never reaches the local, `index_dir`-injecting dispatch path.
//!
//! 4A (this slice) is the walking skeleton: the dispatcher + `fetch`/`status` handlers + the
//! UDS/loopback listener over one read-role connection. 4B adds the bounded worker/read pools, the full
//! operation set, the shared embedder, and real pool health.

pub(crate) mod dispatcher;
pub(crate) mod handlers;
pub(crate) mod listener;
pub(crate) mod serve;

#[cfg(test)]
mod tests;

pub(crate) use dispatcher::SiteDispatcher;
pub(crate) use listener::serve_site_connection;

use jurisearch_core::operation::Operation;

/// Build the 4A walking-skeleton dispatcher: `fetch` (a real `jurisearch-query` builder over a read
/// snapshot) and `status` (server-context health). Every other exposed operation is left UNREGISTERED, so
/// it returns a clear `not_implemented` error while the outer allowlist stays closed to the `Operation`
/// set — and no command ever falls through to the local dispatcher.
pub(crate) fn build_skeleton_dispatcher() -> SiteDispatcher {
    let mut dispatcher = SiteDispatcher::new();
    dispatcher
        .register(Operation::Fetch, Box::new(handlers::FetchHandler))
        .register(Operation::Status, Box::new(handlers::HealthHandler));
    dispatcher
}
