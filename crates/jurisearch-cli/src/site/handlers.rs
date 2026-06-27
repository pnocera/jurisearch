//! work/09 P4 — the site operation handlers. Each is a THIN adapter: validate the wire args, open ONE
//! read snapshot through the server-owned store, validate the writer-owned readiness STAMP on that
//! snapshot ([`ensure_site_readiness`], work/09 P4-4B / codex Q1 — no coverage recompute, no write), then
//! call a side-effect-free `jurisearch-query` builder (or a snapshot-bound storage primitive) and return
//! the result body. No `index_dir`, no write, no local rendering, no environment probing, no online
//! enrichment (a network concern rejected at the boundary). The boundary→input resolution
//! (`resolve_*_input`) is shared with the CLI one-shot adapters, so the two surfaces stay byte-identical.

use jurisearch_core::error::ErrorObject;
use jurisearch_query::{
    CiteInput, FetchInput, build_cite, build_compare, build_context, build_fetch, build_related,
    build_search, enforce_strict_citation, parse_citation_target, storage_error_object,
};
use jurisearch_storage::citation::{CitationLookupQuery, citation_lookup_in_snapshot};
use jurisearch_storage::ingest_accounting::load_query_readiness_in_snapshot;
use jurisearch_storage::query::ReadSnapshot;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    CiteRequest, CompareRequest, ContextRequest, RelatedRequest, SearchRequest, parse_storage_json,
    resolve_compare_input, resolve_context_input, resolve_related_input, resolve_search_input,
    today_utc, validate_as_of, validate_search_common,
};

use super::dispatcher::{OperationHandler, ServerContext};

/// The site read gate (work/09 P4-4B, codex Q1): validate the writer-owned `query_readiness` stamp on
/// the request's OWN snapshot before any builder runs — no coverage recompute, no write, no second
/// connection. A `public`/unstamped/stale/multi-corpus topology fails closed here, so every served read
/// honours the P3A writer-owned-readiness contract. (Diagnostic `status` does NOT gate — it reports the
/// topology, including a not-ready one.)
fn ensure_site_readiness(snapshot: &mut dyn ReadSnapshot) -> Result<(), ErrorObject> {
    load_query_readiness_in_snapshot(snapshot)
        .map(|_| ())
        .map_err(storage_error_object)
}

/// The site `fetch` wire args. STRICT (`deny_unknown_fields`) and strongly typed: `ids` must be an array
/// of strings, and an unsupported option (`part`/`online`/…) is REJECTED rather than silently dropped —
/// a caller is never told an option was honored when the site surface ignored it. (The site surface is
/// base fetch only; the `--part`/online decision-zone overlay is a client/online concern.)
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SiteFetchArgs {
    ids: Vec<String>,
}

/// `fetch`: exact, version-pinned document fetch over a read snapshot.
pub(crate) struct FetchHandler;

impl OperationHandler for FetchHandler {
    fn handle(&self, ctx: &ServerContext, args: &Value) -> Result<Value, ErrorObject> {
        let parsed: SiteFetchArgs = serde_json::from_value(args.clone())
            .map_err(|error| ErrorObject::bad_input(format!("invalid fetch args: {error}")))?;
        if parsed.ids.is_empty() {
            return Err(ErrorObject::bad_input(
                "fetch requires at least one stable ID",
            ));
        }
        let mut snapshot = ctx.store.begin_snapshot().map_err(storage_error_object)?;
        ensure_site_readiness(&mut *snapshot)?;
        build_fetch(
            &FetchInput {
                document_ids: parsed.ids,
            },
            &mut *snapshot,
        )
    }
}

/// `search`: hybrid/bm25/dense retrieval over the read snapshot. Rejects `--zone` (Cassation-only
/// client/online concern, not the shared read path); shares boundary resolution with the CLI adapter.
pub(crate) struct SearchHandler;

impl OperationHandler for SearchHandler {
    fn handle(&self, ctx: &ServerContext, args: &Value) -> Result<Value, ErrorObject> {
        let req: SearchRequest = serde_json::from_value(args.clone())
            .map_err(|error| ErrorObject::bad_input(format!("invalid search args: {error}")))?;
        if req.zone.is_some() {
            return Err(ErrorObject::bad_input(
                "zone search is not available on the site service (a Cassation-only client/online \
                 concern); omit `zone`",
            ));
        }
        validate_search_common(&req)?;
        let input = resolve_search_input(&req)?;
        let mut snapshot = ctx.store.begin_snapshot().map_err(storage_error_object)?;
        ensure_site_readiness(&mut *snapshot)?;
        build_search(&input, &mut *snapshot, Some(ctx.embedder))
    }
}

/// `cite`: citation-state classification over the read snapshot. Rejects `online: true` (a Légifrance
/// network probe is a client/online concern, never run on the shared read service — codex Q2).
pub(crate) struct CiteHandler;

impl OperationHandler for CiteHandler {
    fn handle(&self, ctx: &ServerContext, args: &Value) -> Result<Value, ErrorObject> {
        let req: CiteRequest = serde_json::from_value(args.clone())
            .map_err(|error| ErrorObject::bad_input(format!("invalid cite args: {error}")))?;
        if req.online {
            return Err(ErrorObject::bad_input(
                "online citation verification is not available on the site service (a client/online \
                 concern); omit `online`",
            ));
        }
        if req.cite.trim().is_empty() {
            return Err(ErrorObject::bad_input("cite requires a non-empty citation"));
        }
        validate_as_of(req.as_of.as_deref())?;
        let parsed = parse_citation_target(&req.cite);
        let effective_as_of = req.as_of.clone().unwrap_or_else(today_utc);
        let mut snapshot = ctx.store.begin_snapshot().map_err(storage_error_object)?;
        ensure_site_readiness(&mut *snapshot)?;
        // Only a resolvable citation reads (a malformed one is classified locally); the gate already ran.
        let mut lookup = json!({ "matches": [] });
        if let Some(lookup_target) = parsed.lookup() {
            let response = citation_lookup_in_snapshot(
                &mut *snapshot,
                &CitationLookupQuery {
                    lookup: lookup_target,
                    limit: 25,
                },
            )
            .map_err(storage_error_object)?;
            lookup = parse_storage_json(&response)?;
        }
        let input = CiteInput {
            cite: req.cite.clone(),
            parsed,
            effective_as_of,
            requested_as_of: req.as_of.clone(),
            strict: req.strict,
            // The site never probes online; the response's online block stays `requested:false`.
            online_requested: false,
        };
        let response = build_cite(&input, &lookup);
        enforce_strict_citation(&response, &req.cite, req.strict)?;
        Ok(response)
    }
}

/// `context`: structural ancestry/siblings over the read snapshot.
pub(crate) struct ContextHandler;

impl OperationHandler for ContextHandler {
    fn handle(&self, ctx: &ServerContext, args: &Value) -> Result<Value, ErrorObject> {
        let req: ContextRequest = serde_json::from_value(args.clone())
            .map_err(|error| ErrorObject::bad_input(format!("invalid context args: {error}")))?;
        let input = resolve_context_input(&req)?;
        let mut snapshot = ctx.store.begin_snapshot().map_err(storage_error_object)?;
        ensure_site_readiness(&mut *snapshot)?;
        build_context(&input, &mut *snapshot)
    }
}

/// `related`: depth-1 graph neighbours over the read snapshot.
pub(crate) struct RelatedHandler;

impl OperationHandler for RelatedHandler {
    fn handle(&self, ctx: &ServerContext, args: &Value) -> Result<Value, ErrorObject> {
        let req: RelatedRequest = serde_json::from_value(args.clone())
            .map_err(|error| ErrorObject::bad_input(format!("invalid related args: {error}")))?;
        let input = resolve_related_input(&req)?;
        let mut snapshot = ctx.store.begin_snapshot().map_err(storage_error_object)?;
        ensure_site_readiness(&mut *snapshot)?;
        build_related(&input, &mut *snapshot)
    }
}

/// `compare`: aligned bm25/dense/hybrid retriever comparison over the read snapshot (uses the embedder).
pub(crate) struct CompareHandler;

impl OperationHandler for CompareHandler {
    fn handle(&self, ctx: &ServerContext, args: &Value) -> Result<Value, ErrorObject> {
        let req: CompareRequest = serde_json::from_value(args.clone())
            .map_err(|error| ErrorObject::bad_input(format!("invalid compare args: {error}")))?;
        let input = resolve_compare_input(&req)?;
        let mut snapshot = ctx.store.begin_snapshot().map_err(storage_error_object)?;
        ensure_site_readiness(&mut *snapshot)?;
        build_compare(&input, &mut *snapshot, ctx.embedder)
    }
}

/// `status`: the site service's HEALTH response (NOT the local `status` payload, which is
/// `index_dir`-centric and probes model/cache state). Diagnostic only — it reports the true served
/// topology, the writer-owned readiness stamp (without ever recomputing it), and the bounded read-model
/// sizing. It NEVER gates: a not-ready DB still returns a (not-ready) health body so an operator can see
/// it. `max_read_connections` is the worker count (the hard bound on simultaneous read-role connections).
pub(crate) struct HealthHandler {
    pub(crate) max_read_connections: usize,
}

impl OperationHandler for HealthHandler {
    fn handle(&self, ctx: &ServerContext, _args: &Value) -> Result<Value, ErrorObject> {
        let mut snapshot = ctx.store.begin_snapshot().map_err(storage_error_object)?;
        let corpora: Vec<Value> = snapshot
            .active_corpora()
            .iter()
            .map(|corpus| {
                json!({
                    "corpus": corpus.corpus,
                    "generation": corpus.generation,
                    "schema": corpus.schema,
                    "sequence": corpus.sequence,
                    "fingerprint": corpus.fingerprint,
                })
            })
            .collect();
        // P3A readiness is single-corpus; P3C lifted search fan-out. Report the TRUE topology — including
        // the distinct zero-corpus state (an unactivated site DB must NOT look ready) — never a faked
        // aggregate.
        let multi_corpus_readiness = match corpora.len() {
            0 => "no_active_corpus",
            1 => "single_corpus",
            _ => "deferred",
        };
        // Probe the writer-owned readiness stamp WITHOUT gating: report ready / not-ready + the reason so
        // an operator can diagnose a topology the query handlers would refuse. Never recomputes coverage.
        let readiness = match load_query_readiness_in_snapshot(&mut *snapshot) {
            Ok(report) => json!({
                "ready": true,
                "projection_coverage": report.projection_coverage,
                "embedding_coverage": report.embedding_coverage,
            }),
            Err(error) => json!({ "ready": false, "reason": error.to_string() }),
        };
        Ok(json!({
            "service": "jurisearch-site",
            "active_corpora": corpora,
            "multi_corpus_readiness": multi_corpus_readiness,
            "readiness": readiness,
            // The bounded-worker read model (work/09 P4-4B): worker count == the hard upper bound on
            // simultaneous read-role connections (each request opens + drops one). NOT a reuse pool.
            "read_pool": {
                "mode": "bounded_worker_per_request_connection",
                "max_workers": self.max_read_connections,
                "max_read_connections": self.max_read_connections,
            },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::site::testkit::PanicEmbedder;
    use jurisearch_query::QueryEmbedder;
    use jurisearch_storage::query::{ActiveCorpus, QueryStore, ReadSnapshot};
    use jurisearch_storage::runtime::StorageError;

    /// A store whose snapshot has NO active corpora and is never read — enough to exercise arg-parsing
    /// rejections (which fail before any read) and the zero-corpus health branch.
    struct EmptyStore;
    impl QueryStore for EmptyStore {
        fn begin_snapshot(&self) -> Result<Box<dyn ReadSnapshot + '_>, StorageError> {
            Ok(Box::new(EmptySnapshot))
        }
    }
    struct EmptySnapshot;
    impl ReadSnapshot for EmptySnapshot {
        fn read_text(&mut self, _sql: &str) -> Result<String, StorageError> {
            // No active corpus → the readiness gate fails before any stamp read; arg-rejection tests
            // fail even earlier (before the snapshot). So a read here is a test bug.
            panic!("a rejected request must not read");
        }
        fn read_text_for_corpus(
            &mut self,
            _corpus: &ActiveCorpus,
            _sql: &str,
        ) -> Result<String, StorageError> {
            panic!("a rejected request must not read");
        }
        fn active_corpora(&self) -> &[ActiveCorpus] {
            &[]
        }
    }

    fn ctx<'a>(store: &'a EmptyStore, embedder: &'a PanicEmbedder) -> ServerContext<'a> {
        ServerContext {
            store,
            embedder: embedder as &dyn QueryEmbedder,
        }
    }

    fn fetch_error(args: Value) -> ErrorObject {
        let store = EmptyStore;
        let embedder = PanicEmbedder;
        FetchHandler
            .handle(&ctx(&store, &embedder), &args)
            .expect_err("the malformed/unsupported fetch args must be rejected")
    }

    #[test]
    fn fetch_rejects_mixed_type_ids() {
        // A non-string element is NOT silently dropped — the whole request is invalid.
        let error = fetch_error(json!({"ids": ["cass:X", 123]}));
        assert!(
            error.message.contains("invalid fetch args"),
            "{}",
            error.message
        );
    }

    #[test]
    fn fetch_rejects_all_non_string_ids() {
        let error = fetch_error(json!({"ids": [123, 456]}));
        assert!(
            error.message.contains("invalid fetch args"),
            "{}",
            error.message
        );
    }

    #[test]
    fn fetch_rejects_unsupported_options() {
        // The site surface is base fetch only; `part`/`online` are not silently dropped.
        assert!(
            fetch_error(json!({"ids": ["cass:X"], "part": "motivations"}))
                .message
                .contains("invalid fetch args")
        );
        assert!(
            fetch_error(json!({"ids": ["cass:X"], "online": true}))
                .message
                .contains("invalid fetch args")
        );
    }

    #[test]
    fn fetch_rejects_an_empty_id_list() {
        assert!(
            fetch_error(json!({"ids": []}))
                .message
                .contains("at least one")
        );
    }

    #[test]
    fn search_rejects_zone_at_the_boundary() {
        // `--zone` is a Cassation-only client concern; the site search surface rejects it before any read.
        let store = EmptyStore;
        let embedder = PanicEmbedder;
        let error = SearchHandler
            .handle(
                &ctx(&store, &embedder),
                &json!({"query": "x", "zone": "motivations"}),
            )
            .expect_err("a zone search must be rejected by the site service");
        assert!(error.message.contains("zone"), "{}", error.message);
    }

    #[test]
    fn cite_rejects_online_at_the_boundary() {
        // The Légifrance network probe is a client/online concern; the site cite surface rejects it.
        let store = EmptyStore;
        let embedder = PanicEmbedder;
        let error = CiteHandler
            .handle(
                &ctx(&store, &embedder),
                &json!({"cite": "article 1240", "online": true}),
            )
            .expect_err("an online cite must be rejected by the site service");
        assert!(error.message.contains("online"), "{}", error.message);
    }

    #[test]
    fn health_reports_no_active_corpus_for_an_unactivated_database() {
        let store = EmptyStore;
        let embedder = PanicEmbedder;
        let result = HealthHandler {
            max_read_connections: 4,
        }
        .handle(&ctx(&store, &embedder), &json!({}))
        .expect("health ok");
        assert_eq!(result["active_corpora"].as_array().map(Vec::len), Some(0));
        assert_eq!(
            result["multi_corpus_readiness"].as_str(),
            Some("no_active_corpus"),
            "zero corpora must NOT look single-corpus/ready"
        );
        assert_eq!(result["readiness"]["ready"].as_bool(), Some(false));
        assert_eq!(
            result["read_pool"]["max_read_connections"].as_u64(),
            Some(4)
        );
    }
}
