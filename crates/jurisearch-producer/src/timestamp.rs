//! A dependency-free RFC3339 (UTC) clock for build/manifest timestamps.
//!
//! The package/manifest builders take `created_at` / `generated_at` as caller-supplied strings (so they
//! stay clock-free and deterministic). The producer supplies a real wall-clock RFC3339 here.

use std::time::{SystemTime, UNIX_EPOCH};

/// The current time as an RFC3339 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`).
#[must_use]
pub fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339_from_unix(secs)
}

/// Format whole UNIX seconds (UTC) as an RFC3339 timestamp. Uses Howard Hinnant's civil-from-days
/// algorithm so it is correct across all years without a date library.
#[must_use]
pub fn rfc3339_from_unix(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let secs_of_day = unix_secs % 86_400;
    let (hour, minute, second) = (
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60,
    );

    // civil_from_days: days since 1970-01-01 -> (year, month, day).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_epochs_format_correctly() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix(946_684_800), "2000-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix(1_782_734_400), "2026-06-29T12:00:00Z");
    }
}
