use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadInput,
    NoResults,
    IndexUnavailable,
    DependencyUnavailable,
    Upstream,
    NotImplemented,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorObject {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

impl ErrorObject {
    pub fn bad_input(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::BadInput,
            message: message.into(),
            suggestions: vec![
                "Run `jurisearch help agent` for accepted commands and flags.".into(),
            ],
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: ErrorCode::Internal,
            message: message.into(),
            suggestions: Vec::new(),
        }
    }

    pub fn not_implemented(command: &str) -> Self {
        Self {
            code: ErrorCode::NotImplemented,
            message: format!(
                "`{command}` is registered in the agent contract but is not implemented in this Phase 0 scaffold yet."
            ),
            suggestions: vec![
                "Use `jurisearch help schema --json` to inspect the compiled contract.".into(),
                "Follow IMPLEMENTATION_PLAN.md §10 for the next execution slice.".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessExit {
    Ok = 0,
    User = 2,
    Local = 3,
    Dependency = 4,
    Upstream = 5,
}

impl ProcessExit {
    pub fn code(self) -> i32 {
        self as i32
    }
}

impl From<ErrorCode> for ProcessExit {
    fn from(code: ErrorCode) -> Self {
        match code {
            ErrorCode::BadInput | ErrorCode::NoResults => Self::User,
            ErrorCode::IndexUnavailable | ErrorCode::NotImplemented => Self::Local,
            ErrorCode::DependencyUnavailable | ErrorCode::Internal => Self::Dependency,
            ErrorCode::Upstream => Self::Upstream,
        }
    }
}

#[derive(Debug, Error)]
#[error("{object:?}")]
pub struct ContractError {
    pub object: ErrorObject,
}

impl ContractError {
    pub fn new(object: ErrorObject) -> Self {
        Self { object }
    }

    pub fn exit(&self) -> ProcessExit {
        self.object.code.into()
    }
}
