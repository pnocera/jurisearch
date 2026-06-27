use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ErrorObject;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRequest {
    #[serde(default)]
    pub id: Option<Value>,
    pub command: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SessionResponse {
    Ok {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Value>,
        ok: bool,
        result: Value,
    },
    Err {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<Value>,
        ok: bool,
        error: ErrorObject,
    },
}

impl SessionResponse {
    pub fn ok(id: Option<Value>, result: Value) -> Self {
        Self::Ok {
            id,
            ok: true,
            result,
        }
    }

    pub fn err(id: Option<Value>, error: ErrorObject) -> Self {
        Self::Err {
            id,
            ok: false,
            error,
        }
    }

    /// True for the `Ok` variant.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Ok { .. })
    }

    /// The correlation id echoed back, if any (present on both variants).
    pub fn id(&self) -> Option<&Value> {
        match self {
            Self::Ok { id, .. } | Self::Err { id, .. } => id.as_ref(),
        }
    }

    /// The success result body, or `None` on the error variant. Lets a dependency-light renderer
    /// (`jurisearch-render`) unwrap a response without reaching into private fields.
    pub fn result(&self) -> Option<&Value> {
        match self {
            Self::Ok { result, .. } => Some(result),
            Self::Err { .. } => None,
        }
    }

    /// The error object, or `None` on the success variant.
    pub fn error(&self) -> Option<&ErrorObject> {
        match self {
            Self::Err { error, .. } => Some(error),
            Self::Ok { .. } => None,
        }
    }
}
