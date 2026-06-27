//! work/09 P4 — the site operation handlers. Each is a THIN adapter: validate the wire args, open one
//! read snapshot through the server-owned store, call a side-effect-free `jurisearch-query` builder (or a
//! snapshot-bound storage primitive), and return the result body. No `index_dir`, no write, no local
//! rendering, no environment probing.

use jurisearch_core::error::ErrorObject;
use jurisearch_query::{FetchInput, build_fetch, storage_error_object};
use serde::Deserialize;
use serde_json::{Value, json};

use super::dispatcher::{OperationHandler, ServerContext};

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
        build_fetch(
            &FetchInput {
                document_ids: parsed.ids,
            },
            &mut *snapshot,
        )
    }
}

/// `status`: the site service's HEALTH response (NOT the local `status` payload, which is
/// `index_dir`-centric and probes model/cache state). Diagnostic only — it reports the true served
/// topology and never recomputes readiness.
pub(crate) struct HealthHandler;

impl OperationHandler for HealthHandler {
    fn handle(&self, ctx: &ServerContext, _args: &Value) -> Result<Value, ErrorObject> {
        let snapshot = ctx.store.begin_snapshot().map_err(storage_error_object)?;
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
        Ok(json!({
            "service": "jurisearch-site",
            "active_corpora": corpora,
            "multi_corpus_readiness": multi_corpus_readiness,
            // 4A is a single pooled connection; a real read-pool's idle/size counts arrive with 4B.
            "read_pool": { "mode": "size_1" },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            panic!("a rejected fetch must not read");
        }
        fn read_text_for_corpus(
            &mut self,
            _corpus: &ActiveCorpus,
            _sql: &str,
        ) -> Result<String, StorageError> {
            panic!("a rejected fetch must not read");
        }
        fn active_corpora(&self) -> &[ActiveCorpus] {
            &[]
        }
    }

    fn fetch_error(args: Value) -> ErrorObject {
        let ctx = ServerContext { store: &EmptyStore };
        FetchHandler
            .handle(&ctx, &args)
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
    fn health_reports_no_active_corpus_for_an_unactivated_database() {
        let ctx = ServerContext { store: &EmptyStore };
        let result = HealthHandler.handle(&ctx, &json!({})).expect("health ok");
        assert_eq!(result["active_corpora"].as_array().map(Vec::len), Some(0));
        assert_eq!(
            result["multi_corpus_readiness"].as_str(),
            Some("no_active_corpus"),
            "zero corpora must NOT look single-corpus/ready"
        );
    }
}
