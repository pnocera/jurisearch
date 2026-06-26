//! `jurisearch-package` — the producer↔client contract crate (plan P0, design §5–§6, §8, §10, §11).
//!
//! This crate is the **single source of truth** for the agreement between the central producer and
//! the read-only clients (conception §3 DRY): the package/manifest types, the event-kind and
//! reject-code vocabularies, the two distinct sequence layers, the compatibility stamps, the
//! identity helpers, deterministic canonicalisation, and the `Signer`/`Verifier` trust seam. Both
//! sides compile against it, so neither can drift.
//!
//! It is a **pure leaf crate**: types + serde + canonicalisation + traits only — **no I/O, no DB,
//! no concrete crypto**. Storage, the CLI, and the client service all depend on it.
//!
//! Module map (→ design section):
//! * [`sequence`] — `ChangeSeq` vs `PackageSequence`, the two non-interchangeable orderings (§5.1).
//! * [`corpus`] — `Corpus` + the `source → corpus` attribution rule (§4.1, P0).
//! * [`event`] — the three event kinds and the `replace_set` payload contract (§5.2, §5.3).
//! * [`package_kind`] — `baseline`/`rebaseline`/`incremental` (§6.1).
//! * [`compat`] — compatibility stamps + the version gate's comparison (§10).
//! * [`identity`] — the two identities + the `response_id` surrogate-key rule (§8.1, §5.2).
//! * [`reject`] — the closed reject-code vocabulary (§6.3).
//! * [`canonical`] — deterministic manifest encoding + digests (§6.2.2, §11.1).
//! * [`crypto`] — `Signer`/`Verifier` traits + the wire `Signature` (§11.2).
//! * [`signed`] — the detached-signature document wrapper (§6.2).
//! * [`manifest`] — the two-tier remote + embedded manifests (§6.2).

pub mod artifact;
pub mod canonical;
pub mod compat;
pub mod corpus;
pub mod crypto;
pub mod event;
pub mod identity;
pub mod manifest;
pub mod package_kind;
pub mod reject;
pub mod sequence;
pub mod signed;

/// The package-format version this build of the contract crate speaks (design §6.2.2
/// `package_format_version`; cross-cutting §6.1 "a format bump is a `minimum_client_version` event").
pub const PACKAGE_FORMAT_VERSION: u32 = 1;

// Re-exports for the common surface, so dependents can `use jurisearch_package::{...}`.
pub use compat::{CompatibilityStamps, Version};
pub use corpus::{AttributionError, Corpus, corpus_for_source};
pub use crypto::{KeyEpoch, KeyId, Signature, Signer, Verifier};
pub use event::{EventKind, ReplaceSet, ReplaceSetGroup, ScopeKind};
pub use identity::{DocumentVersionId, LogicalArticleId, ResponseId};
pub use manifest::{EmbeddedManifest, RemoteManifest};
pub use package_kind::PackageKind;
pub use reject::{RejectCode, RejectError};
pub use sequence::{ChangeSeq, PackageSequence};
pub use signed::Signed;
