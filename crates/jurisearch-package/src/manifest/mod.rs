//! The two-tier signed manifest (design §6.2): the per-corpus [`remote`] listing the client polls,
//! and the per-package [`embedded`] manifest that travels inside each artifact.

pub mod embedded;
pub mod remote;

pub use embedded::EmbeddedManifest;
pub use remote::RemoteManifest;
