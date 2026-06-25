//! Official French legal API client (PISTE / Judilibre + Legifrance): configuration and
//! credential resolution, the `PisteClient` exchange surface, token caching, retry/backoff,
//! and error mapping. The crate root keeps the shared external imports and re-exports the
//! public API; each submodule pulls them via `use crate::*`.

use std::{
    env, fmt,
    time::{Duration, Instant},
};

use jurisearch_core::error::{ErrorCode, ErrorObject};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

mod client;
mod config;
mod error;
mod retry;

use crate::config::*;
use crate::error::*;
use crate::retry::*;

pub use client::{OfficialApiExchange, OfficialApiOutcome, PisteClient};
pub use config::{OfficialApiConfig, PisteEnvironment};
pub use error::OfficialApiError;
pub use retry::RetryPolicy;

#[cfg(test)]
mod tests;
