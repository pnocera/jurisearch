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
}
