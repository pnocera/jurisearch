//! work/09 P3B — the side-effect-free read-response builders + the query embedder seam.
//!
//! A builder takes a small, validated **input struct**, a [`ReadSnapshot`](jurisearch_storage::query::ReadSnapshot)
//! (one request = one MVCC snapshot), and — for dense retrieval — a [`QueryEmbedder`], and returns the
//! response body as a [`serde_json::Value`]. It does **no** `index_dir` resolution, **no** Postgres
//! start, and **no** write: every database read goes through the snapshot. The CLI `*_payload` functions
//! and (P4) the site query service are thin adapters over these builders — one response-building
//! authority, so both render byte-identical responses while the service carries none of the CLI's side
//! effects.
//!
//! This crate is dependency-light by construction: it depends only on `jurisearch-core` (the
//! `ErrorObject` vocabulary) and `jurisearch-storage` (the snapshot + read SQL), never on the CLI,
//! embedder runtime, or ingest stack.

pub mod builders;
pub mod embedder;
pub mod errors;

pub use builders::{
    CompareInput, ContextInput, FetchInput, RelatedInput, build_compare, build_context,
    build_fetch, build_related,
};
pub use embedder::{QueryEmbedder, QueryEmbedding};
pub use errors::{
    dependency_unavailable, index_unavailable, no_results, parse_storage_json, storage_error_object,
};
