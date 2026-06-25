//! Date/id validation and calendar math for LEGI records.

use super::*;

pub(super) fn validate_id(
    field: &'static str,
    value: &str,
    prefix: &'static str,
    expected: &'static str,
) -> Result<(), LegiParseError> {
    let suffix = value
        .strip_prefix(prefix)
        .ok_or(LegiParseError::InvalidId {
            field,
            value: value.to_owned(),
            expected,
        })?;
    if suffix.len() == 12 && suffix.chars().all(|character| character.is_ascii_digit()) {
        Ok(())
    } else {
        Err(LegiParseError::InvalidId {
            field,
            value: value.to_owned(),
            expected,
        })
    }
}

pub(super) fn normalize_required_date(
    field: &'static str,
    value: &str,
) -> Result<String, LegiParseError> {
    validate_date(field, value)?;
    Ok(value.to_owned())
}

pub(super) fn normalize_end_date(
    field: &'static str,
    value: &str,
) -> Result<Option<String>, LegiParseError> {
    validate_date(field, value)?;
    if matches!(value, "2999-01-01" | "2999-12-31") {
        Ok(None)
    } else {
        Ok(Some(value.to_owned()))
    }
}

pub(super) fn validate_date(field: &'static str, value: &str) -> Result<(), LegiParseError> {
    let bytes = value.as_bytes();
    let valid_shape = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit());
    if !valid_shape {
        return Err(LegiParseError::InvalidDate {
            field,
            value: value.to_owned(),
        });
    }
    let year = value[0..4].parse::<u16>().unwrap_or_default();
    let month = value[5..7].parse::<u8>().unwrap_or_default();
    let day = value[8..10].parse::<u8>().unwrap_or_default();
    if day > 0 && day <= days_in_month(year, month).unwrap_or_default() {
        Ok(())
    } else {
        Err(LegiParseError::InvalidDate {
            field,
            value: value.to_owned(),
        })
    }
}

fn days_in_month(year: u16, month: u8) -> Option<u8> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if is_leap_year(year) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

fn is_leap_year(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}
