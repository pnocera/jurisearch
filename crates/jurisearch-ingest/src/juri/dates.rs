//! Date/uid validation and calendar math for JURI decisions.

use super::*;

pub(super) fn validate_uid(value: &str, family: JuriFamily) -> Result<(), JuriParseError> {
    let prefix = family.uid_prefix();
    let expected: &'static str = match family {
        JuriFamily::Judicial => "JURITEXT[0-9]{12}",
        JuriFamily::Administrative => "CETATEXT[0-9]{12}",
    };
    let suffix = value.strip_prefix(prefix).ok_or(JuriParseError::InvalidId {
        field: "ID",
        value: value.to_owned(),
        expected,
    })?;
    if suffix.len() == 12 && suffix.chars().all(|character| character.is_ascii_digit()) {
        Ok(())
    } else {
        Err(JuriParseError::InvalidId {
            field: "ID",
            value: value.to_owned(),
            expected,
        })
    }
}

pub(super) fn validate_date_field(field: &'static str, value: &str) -> Result<(), JuriParseError> {
    validate_iso_date(value).map_err(|()| JuriParseError::InvalidDate {
        field,
        value: value.to_owned(),
    })
}

pub(super) fn validate_iso_date(value: &str) -> Result<(), ()> {
    let bytes = value.as_bytes();
    let valid_shape = bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit());
    if !valid_shape {
        return Err(());
    }
    let year = value[0..4].parse::<u16>().unwrap_or_default();
    let month = value[5..7].parse::<u8>().unwrap_or_default();
    let day = value[8..10].parse::<u8>().unwrap_or_default();
    if day > 0 && day <= days_in_month(year, month).unwrap_or_default() {
        Ok(())
    } else {
        Err(())
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
