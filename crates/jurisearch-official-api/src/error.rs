//! OfficialApiError variants + mapping helpers (official_api_error, truncated_body).

use crate::*;

#[derive(Debug, Error)]
pub enum OfficialApiError {
    #[error("missing official API credential; checked {names:?}")]
    MissingCredential { names: &'static [&'static str] },
    #[error("official API rate limited the request with status 429")]
    RateLimited {
        retry_after: Option<String>,
        body: String,
    },
    #[error("official API returned HTTP status {status}: {body}")]
    UpstreamStatus { status: u16, body: String },
    #[error("official API transport failed: {0}")]
    Transport(String),
    #[error("official API response was invalid: {0}")]
    InvalidResponse(String),
}

impl OfficialApiError {
    #[must_use]
    pub fn to_error_object(&self) -> ErrorObject {
        match self {
            Self::MissingCredential { names } => ErrorObject {
                code: ErrorCode::DependencyUnavailable,
                message: self.to_string(),
                suggestions: vec![format!(
                    "Set one of {} in the environment or configure an OS keyring entry.",
                    names.join(", ")
                )],
            },
            Self::RateLimited { retry_after, .. } => ErrorObject {
                code: ErrorCode::Upstream,
                message: match retry_after {
                    Some(retry_after) => {
                        format!("official API rate limited the request; retry after {retry_after}")
                    }
                    None => "official API rate limited the request".to_owned(),
                },
                suggestions: vec!["Back off and retry later; prefer bulk dumps for full builds.".into()],
            },
            Self::UpstreamStatus { .. } | Self::Transport(_) | Self::InvalidResponse(_) => {
                ErrorObject {
                    code: ErrorCode::Upstream,
                    message: self.to_string(),
                    suggestions: vec![
                        "Check official API availability, credentials, subscription, and rate limits."
                            .into(),
                    ],
                }
            }
        }
    }
}

pub(crate) fn official_api_error(error: ureq::Error) -> OfficialApiError {
    match error {
        ureq::Error::Status(429, response) => {
            let retry_after = response.header("Retry-After").map(str::to_owned);
            let body = response.into_string().unwrap_or_default();
            OfficialApiError::RateLimited { retry_after, body }
        }
        ureq::Error::Status(status, response) => {
            let body = truncated_body(response.into_string().unwrap_or_default());
            OfficialApiError::UpstreamStatus { status, body }
        }
        other => OfficialApiError::Transport(other.to_string()),
    }
}

pub(crate) fn truncated_body(body: String) -> String {
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut truncated = body.chars().take(UPSTREAM_BODY_LIMIT).collect::<String>();
    if body.chars().count() > UPSTREAM_BODY_LIMIT {
        truncated.push_str("...");
    }
    truncated
}
