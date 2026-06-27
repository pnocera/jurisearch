//! work/09 P4 — the site query service: a server-owned, read-only-safe, allowlist-by-construction
//! dispatcher over the versioned JSONL site transport. Disjoint from the local `serve.rs` session loop;
//! a site request never reaches the local, `index_dir`-injecting dispatch path.
//!
//! 4A was the walking skeleton (dispatcher + `fetch`/`status` + a sequential listener). 4B (this slice)
//! registers the full operation set (search/fetch/cite/related/context/compare + health), moves the
//! `search`/`cite` response construction into `jurisearch-query`, gates every query on the writer-owned
//! readiness stamp (snapshot-bound), and serves connections from a bounded worker pool with a shared,
//! `Send + Sync` query embedder and a bounded in-flight-embed semaphore.

pub(crate) mod dispatcher;
pub(crate) mod handlers;
pub(crate) mod listener;
pub(crate) mod serve;

#[cfg(test)]
mod tests;

pub(crate) use dispatcher::SiteDispatcher;
pub(crate) use listener::serve_site_connection;

use jurisearch_core::operation::Operation;

/// Build the full site dispatcher: every exposed [`Operation`] mapped to its handler (the handler map IS
/// the allowlist). `max_read_connections` is the bounded-worker read bound, reported by health.
pub(crate) fn build_dispatcher(max_read_connections: usize) -> SiteDispatcher {
    let mut dispatcher = SiteDispatcher::new();
    dispatcher
        .register(Operation::Search, Box::new(handlers::SearchHandler))
        .register(Operation::Fetch, Box::new(handlers::FetchHandler))
        .register(Operation::Cite, Box::new(handlers::CiteHandler))
        .register(Operation::Related, Box::new(handlers::RelatedHandler))
        .register(Operation::Context, Box::new(handlers::ContextHandler))
        .register(Operation::Compare, Box::new(handlers::CompareHandler))
        .register(
            Operation::Status,
            Box::new(handlers::HealthHandler {
                max_read_connections,
            }),
        );
    dispatcher
}

#[cfg(test)]
pub(crate) mod testkit {
    //! Shared test doubles for the site query service.
    use jurisearch_core::error::ErrorObject;
    use jurisearch_query::{QueryEmbedder, QueryEmbedding};

    /// A query embedder that PANICS if invoked — for handler/dispatcher/listener tests whose paths must
    /// never reach a dense embed (arg-rejected, lexical-only, or framing-failure).
    pub(crate) struct PanicEmbedder;
    impl QueryEmbedder for PanicEmbedder {
        fn embed(&self, _text: &str) -> Result<QueryEmbedding, ErrorObject> {
            panic!("this test path must never embed");
        }
    }

    /// A query embedder returning a fixed pgvector literal under a CONFIGURABLE fingerprint — lets an
    /// e2e test drive the dense path and force a fingerprint mismatch through the site dispatcher.
    pub(crate) struct StubEmbedder {
        pub(crate) fingerprint: String,
        pub(crate) dimension: usize,
    }
    impl QueryEmbedder for StubEmbedder {
        fn embed(&self, _text: &str) -> Result<QueryEmbedding, ErrorObject> {
            let literal = format!("[{}]", vec!["0.01"; self.dimension].join(","));
            Ok(QueryEmbedding {
                literal,
                fingerprint: self.fingerprint.clone(),
            })
        }
    }
}
