#![recursion_limit = "512"]

pub mod contract;
pub mod envelope;
pub mod error;
pub mod eval;
pub mod expand;
pub mod operation;
pub mod retrieval;
pub mod schema;
pub mod session;
pub mod site_request;

pub const SCHEMA_VERSION: &str = "1";
