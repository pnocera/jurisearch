//! Shared release-gate mechanics: dotted JSON-pointer accessors into artifacts.

use crate::*;

pub(crate) fn artifact_pointer_value<'a>(value: &'a Value, dotted_path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in dotted_path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

pub(crate) fn artifact_pointer_str<'a>(value: &'a Value, dotted_path: &str) -> Option<&'a str> {
    artifact_pointer_value(value, dotted_path)?.as_str()
}

pub(crate) fn artifact_pointer_f64(value: &Value, dotted_path: &str) -> Option<f64> {
    artifact_pointer_value(value, dotted_path)?.as_f64()
}
