//! Retrieval command payloads, one submodule per command (search/zone/cite/context/
//! related/compare/expand; fetch lands in Phase 3c). Submodules are re-exported so the
//! dispatcher and session wrappers call `crate::<fn>` unchanged. `validate_as_of` lives
//! here (shared by cite/context and, in status, diff) and validates against the calendar
//! helpers in `crate::date`.

use crate::*;

pub(crate) mod cite;
pub(crate) mod compare;
pub(crate) mod context;
pub(crate) mod expand;
pub(crate) mod related;
pub(crate) mod search;
pub(crate) mod zone;

pub(crate) use cite::*;
pub(crate) use compare::*;
pub(crate) use context::*;
pub(crate) use expand::*;
pub(crate) use related::*;
pub(crate) use search::*;
pub(crate) use zone::*;

pub(crate) fn validate_as_of(as_of: Option<&str>) -> Result<(), ErrorObject> {
    if let Some(as_of) = as_of
        && !is_valid_iso_date(as_of)
    {
        return Err(ErrorObject::bad_input(format!(
            "--as-of must be a valid ISO date in YYYY-MM-DD format, got `{as_of}`"
        )));
    }
    Ok(())
}
