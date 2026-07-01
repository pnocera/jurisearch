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

/// Format whole UNIX seconds (UTC) as a DILA-style compact `YYYYMMDDHHMMSS` (exactly 14 digits). The
/// INVERSE of [`unix_from_compact_archive_timestamp`]; shares Howard Hinnant's civil-from-days algorithm
/// with [`rfc3339_from_unix`] so it is correct across all years without a date library. Used by the
/// `--from-db` cursor seed to write a valid "now" compact anchor into the synthetic completed ingest run.
#[must_use]
pub fn compact_from_unix(unix_secs: u64) -> String {
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

    format!("{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}")
}

/// The current UTC time as a DILA compact timestamp (`YYYYMMDDHHMMSS`). The "now anchor" a `--from-db`
/// cursor seed writes so a future delta-only ingest resumes from this instant (the operator-accepted gap).
#[must_use]
pub fn now_compact() -> String {
    compact_from_unix(now_unix())
}

/// The current time as whole UNIX seconds (UTC). The injectable-`now` seam for age/freshness math.
#[must_use]
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Parse an RFC3339 UTC timestamp in the exact `YYYY-MM-DDTHH:MM:SSZ` shape this module emits back to
/// whole UNIX seconds, or `None` if it is not that shape. Inverse of [`rfc3339_from_unix`] (Howard
/// Hinnant's days-from-civil), so producer timestamps can be aged WITHOUT a date dependency.
#[must_use]
pub fn unix_from_rfc3339(ts: &str) -> Option<u64> {
    let (date, rest) = ts.split_once('T')?;
    let time = rest.strip_suffix('Z')?;
    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    if d.next().is_some() {
        return None;
    }
    let mut t = time.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let second: i64 = t.next()?.parse().ok()?;
    if t.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    // days_from_civil: (year, month, day) -> days since 1970-01-01.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second;
    u64::try_from(secs).ok()
}

/// Parse a DILA archive compact timestamp (`YYYYMMDDHHMMSS`, exactly 14 digits, UTC) to whole UNIX
/// seconds, or `None` if it is not that shape. This is the compact analogue of [`unix_from_rfc3339`]
/// for the archive "cursor" (a `ArchiveTimestamp::compact()` value), used to age a delta-only ingest
/// cursor against DILA's server-side delta retention. It does NOT route through [`unix_from_rfc3339`];
/// the compact form carries no separators, so it is parsed directly (Howard Hinnant days-from-civil).
#[must_use]
pub fn unix_from_compact_archive_timestamp(compact: &str) -> Option<u64> {
    if compact.len() != 14 || !compact.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: i64 = compact[0..4].parse().ok()?;
    let month: i64 = compact[4..6].parse().ok()?;
    let day: i64 = compact[6..8].parse().ok()?;
    let hour: i64 = compact[8..10].parse().ok()?;
    let minute: i64 = compact[10..12].parse().ok()?;
    let second: i64 = compact[12..14].parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }

    // days_from_civil: (year, month, day) -> days since 1970-01-01.
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if month > 2 { month - 3 } else { month + 9 }; // [0, 11]
    let doy = (153 * mp + 2) / 5 + day - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second;
    u64::try_from(secs).ok()
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

    #[test]
    fn rfc3339_round_trips_through_unix() {
        for secs in [0, 946_684_800, 1_782_734_400, 1_751_200_200] {
            assert_eq!(unix_from_rfc3339(&rfc3339_from_unix(secs)), Some(secs));
        }
        assert_eq!(unix_from_rfc3339("not-a-timestamp"), None);
        assert_eq!(unix_from_rfc3339("2026-13-01T00:00:00Z"), None);
    }

    #[test]
    fn compact_archive_timestamp_parses_to_unix() {
        // A known DILA delta timestamp: 2026-06-29T12:00:00Z == 1_782_734_400.
        assert_eq!(
            unix_from_compact_archive_timestamp("20260629120000"),
            Some(1_782_734_400)
        );
        assert_eq!(
            unix_from_compact_archive_timestamp("19700101000000"),
            Some(0)
        );
        // Agrees with the RFC3339 helper for the same instant (both UTC).
        assert_eq!(
            unix_from_compact_archive_timestamp("20000101000000"),
            unix_from_rfc3339("2000-01-01T00:00:00Z")
        );
    }

    #[test]
    fn compact_from_unix_round_trips_and_is_14_digits() {
        for secs in [0, 946_684_800, 1_782_734_400, 1_751_200_200] {
            let compact = compact_from_unix(secs);
            assert_eq!(compact.len(), 14, "compact is exactly 14 digits");
            assert!(compact.bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(
                unix_from_compact_archive_timestamp(&compact),
                Some(secs),
                "compact_from_unix is the inverse of unix_from_compact_archive_timestamp"
            );
        }
        // A known instant: 2026-06-29T12:00:00Z.
        assert_eq!(compact_from_unix(1_782_734_400), "20260629120000");
    }

    #[test]
    fn compact_archive_timestamp_rejects_malformed() {
        assert_eq!(unix_from_compact_archive_timestamp(""), None);
        assert_eq!(unix_from_compact_archive_timestamp("2026062912000"), None); // 13 digits
        assert_eq!(unix_from_compact_archive_timestamp("202606291200000"), None); // 15 digits
        assert_eq!(unix_from_compact_archive_timestamp("2026-06-29T120"), None); // separators
        assert_eq!(unix_from_compact_archive_timestamp("20261329120000"), None); // month 13
        assert_eq!(unix_from_compact_archive_timestamp("20260600120000"), None); // day 00
        assert_eq!(unix_from_compact_archive_timestamp("20260629250000"), None); // hour 25
    }
}
