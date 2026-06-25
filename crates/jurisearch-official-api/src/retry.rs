//! Retry policy + send_with_retry exponential backoff and Retry-After parsing.

use crate::*;

pub(super) const DEFAULT_MAX_RETRIES: u32 = 3;

const DEFAULT_RETRY_BASE_DELAY: Duration = Duration::from_millis(500);

const DEFAULT_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);

/// Retry/backoff policy for transient upstream failures (HTTP 429 and 5xx). Only safe,
/// read-style PISTE requests are issued through it; transport errors are not retried.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Maximum number of retries AFTER the initial attempt.
    pub max_retries: u32,
    /// Base delay for exponential backoff (`base * 2^attempt`).
    pub base_delay: Duration,
    /// Upper bound on any single backoff wait; also caps a `Retry-After` value.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            base_delay: DEFAULT_RETRY_BASE_DELAY,
            max_delay: DEFAULT_RETRY_MAX_DELAY,
        }
    }
}

impl RetryPolicy {
    /// A policy that retries `max_retries` times without sleeping — for deterministic tests.
    #[must_use]
    pub fn immediate(max_retries: u32) -> Self {
        Self {
            max_retries,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        }
    }

    /// Default policy, with `max_retries` overridable via `JURISEARCH_PISTE_MAX_RETRIES`
    /// (e.g. set to `0` to disable retries deterministically in tests/probes).
    #[must_use]
    pub fn from_env() -> Self {
        let mut policy = Self::default();
        if let Ok(value) = env::var("JURISEARCH_PISTE_MAX_RETRIES") {
            if let Ok(max_retries) = value.trim().parse::<u32>() {
                policy.max_retries = max_retries;
            }
        }
        policy
    }
}

/// Send a request, retrying transient upstream failures (HTTP 429 and 5xx) per `policy`.
/// The `send` closure rebuilds and issues the request on each attempt (ureq builders are
/// single-use). Transport errors and non-retryable statuses (e.g. 4xx) are returned immediately.
pub(super) fn send_with_retry<F>(policy: RetryPolicy, mut send: F) -> Result<ureq::Response, ureq::Error>
where
    F: FnMut() -> Result<ureq::Response, ureq::Error>,
{
    let mut attempt: u32 = 0;
    loop {
        match send() {
            Ok(response) => return Ok(response),
            Err(error) => {
                if attempt >= policy.max_retries || !is_retryable_status(&error) {
                    return Err(error);
                }
                let delay = retry_delay(retry_after_from_error(&error), attempt, policy);
                if !delay.is_zero() {
                    std::thread::sleep(delay);
                }
                attempt += 1;
            }
        }
    }
}

fn is_retryable_status(error: &ureq::Error) -> bool {
    matches!(error, ureq::Error::Status(code, _) if *code == 429 || (500..=599).contains(code))
}

pub(super) fn retry_after_from_error(error: &ureq::Error) -> Option<Duration> {
    match error {
        // `Retry-After` is upstream-directed for any retryable status (429 and 5xx both use it).
        ureq::Error::Status(code, response) if *code == 429 || (500..=599).contains(code) => {
            response
                .header("Retry-After")
                .and_then(parse_retry_after_seconds)
        }
        _ => None,
    }
}

/// Backoff for a given attempt (0-based): a `Retry-After` value wins (capped by `max_delay`),
/// otherwise exponential `base_delay * 2^attempt`, capped by `max_delay`.
pub(super) fn retry_delay(retry_after: Option<Duration>, attempt: u32, policy: RetryPolicy) -> Duration {
    if let Some(after) = retry_after {
        return after.min(policy.max_delay);
    }
    let factor = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
    policy
        .base_delay
        .checked_mul(factor)
        .unwrap_or(policy.max_delay)
        .min(policy.max_delay)
}

fn parse_retry_after_seconds(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}
