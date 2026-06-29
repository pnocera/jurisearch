//! `site watchdog` (plan `02` Phase 7, M5-B): a READ-ONLY monitor that detects a STALLED site sync
//! cursor and DISTINGUISHES "the site sync cursor is stuck" from "the producer simply has no new
//! packages".
//!
//! The discrimination is the whole point of the gate, and it is a PURE function ([`classify_watchdog`])
//! over two facts the watchdog reads without mutating anything:
//!   1. the site's APPLIED cursor sequence (+ how long since it last advanced), and
//!   2. the VERIFIED producer head sequence from the signed remote manifest.
//!
//! - `applied == head`  →  [`WatchdogStatus::NoNewPackages`]  (healthy: the site is at the verified head;
//!   the producer has published nothing the site lacks — NOT a stall).
//! - `applied <  head`  →  there ARE packages the site has not applied. It is [`WatchdogStatus::CatchingUp`]
//!   (a normal in-flight catch-up) ONLY when the cursor advanced within the stall window — i.e. its
//!   `applied_at` age is KNOWN and recent. Otherwise (aged past the window, OR the age is unknown because
//!   `applied_at` was missing/unparseable) it FAILS CLOSED to [`WatchdogStatus::StalledCursor`] (the
//!   site-sync fault to alert on) — a behind cursor never reports healthy on an unreadable timestamp.
//! - `applied >  head`  →  [`WatchdogStatus::AheadOfHead`] (the site is on a different/newer feed — a
//!   misconfiguration, surfaced rather than silently ignored).
//!
//! The live runner ([`watchdog_corpus`]) reuses the syncd read primitives (`fetch_verify_manifest`,
//! `read_client_cursor`) and `corpus_status` (a SELECT-only writer query) and NEVER calls `run_catchup`
//! or any mutating path, so monitoring can never advance/repair the cursor it is watching.

use serde::Serialize;

use jurisearch_storage::backend::WriterConnection;
use jurisearch_syncd::{
    DirectoryCatchupSource, corpus_status, fetch_verify_manifest, load_package_verifier,
    read_client_cursor,
};

use crate::config::SiteConfig;
use crate::error::DeployError;

use super::catchup::DEFAULT_URI_BASE;

/// The default stall window: if the site cursor is BEHIND the verified head and has not advanced for at
/// least this long, the watchdog reports a stalled cursor. Two daily sync windows of slack.
pub const DEFAULT_STALL_THRESHOLD_SECS: u64 = 48 * 3_600;

/// The read-only facts the stall decision needs (kept numeric so the decision is unit-tested without a
/// clock, DB, or package source).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchdogObservation {
    /// The site's applied cursor sequence, or `None` when no corpus is installed/active.
    pub applied_sequence: Option<u64>,
    /// The VERIFIED producer head sequence from the signed remote manifest.
    pub producer_head_sequence: u64,
    /// Seconds since the site cursor last advanced (`applied_at` aged against now), or `None` when that
    /// timestamp is unknown (missing or unparseable). Only consulted when the cursor is BEHIND the head,
    /// where `None` FAILS CLOSED to a stall (never a silent catch-up).
    pub cursor_age_secs: Option<u64>,
    /// The stall window — a behind cursor older than this is reported as stalled.
    pub stall_threshold_secs: u64,
}

/// The READ-ONLY watchdog verdict for one corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WatchdogStatus {
    /// No corpus is installed/active yet — there is nothing to watch.
    NoActiveCorpus,
    /// The applied cursor EQUALS the verified head: the site is current and the producer has no new
    /// packages. Healthy — explicitly NOT a stall.
    NoNewPackages,
    /// The applied cursor is BEHIND the verified head AND has not advanced within the stall window: the
    /// site sync cursor is STUCK. This is the alerting state.
    StalledCursor,
    /// The applied cursor is BEHIND the verified head but advanced within the stall window: a normal
    /// in-flight catch-up, not (yet) a stall.
    CatchingUp,
    /// The applied cursor is AHEAD of the verified head: the site is on a different/newer feed than this
    /// producer head (a misconfiguration to surface).
    AheadOfHead,
}

impl WatchdogStatus {
    /// A stable machine code per state.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            WatchdogStatus::NoActiveCorpus => "watchdog.no_active_corpus",
            WatchdogStatus::NoNewPackages => "watchdog.no_new_packages",
            WatchdogStatus::StalledCursor => "watchdog.stalled_cursor",
            WatchdogStatus::CatchingUp => "watchdog.catching_up",
            WatchdogStatus::AheadOfHead => "watchdog.ahead_of_head",
        }
    }

    /// Whether this state should ALERT an operator (only a stuck cursor or a wrong-feed cursor do).
    #[must_use]
    pub fn is_alert(self) -> bool {
        matches!(
            self,
            WatchdogStatus::StalledCursor | WatchdogStatus::AheadOfHead
        )
    }

    /// A one-line human explanation (the cursor↔head relationship that produced this verdict).
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            WatchdogStatus::NoActiveCorpus => "no corpus installed yet — nothing to watch",
            WatchdogStatus::NoNewPackages => {
                "site cursor is AT the verified producer head — the producer has no new packages (healthy)"
            }
            WatchdogStatus::StalledCursor => {
                "site cursor is BEHIND the verified head and has NOT advanced within the stall window \
                 — the site sync cursor is STUCK (alert)"
            }
            WatchdogStatus::CatchingUp => {
                "site cursor is behind the verified head but advanced recently — a normal catch-up"
            }
            WatchdogStatus::AheadOfHead => {
                "site cursor is AHEAD of the verified head — the site is on a different/newer feed (alert)"
            }
        }
    }
}

/// PURE: the stall decision. `applied == head` is the explicit "producer has no new packages" healthy
/// state; only a cursor that is BEHIND the head is a stall candidate. A behind cursor is a normal
/// [`WatchdogStatus::CatchingUp`] ONLY when its `applied_at` age is KNOWN and within the stall window.
///
/// FAIL-CLOSED: a behind cursor whose age is UNKNOWN (the `applied_at` was missing or unparseable) is
/// classified [`WatchdogStatus::StalledCursor`], NEVER `CatchingUp`. A cursor that is genuinely behind
/// the verified producer head must never report healthy just because its timestamp could not be read —
/// that would be the exact false-green the gate forbids. Catch-up is the EVIDENCED state (recent
/// advance), not the default.
#[must_use]
pub fn classify_watchdog(observation: WatchdogObservation) -> WatchdogStatus {
    let Some(applied) = observation.applied_sequence else {
        return WatchdogStatus::NoActiveCorpus;
    };
    use std::cmp::Ordering;
    match applied.cmp(&observation.producer_head_sequence) {
        Ordering::Equal => WatchdogStatus::NoNewPackages,
        Ordering::Greater => WatchdogStatus::AheadOfHead,
        Ordering::Less => match observation.cursor_age_secs {
            // Behind, but the cursor advanced within the window → a normal in-flight catch-up.
            Some(age) if age < observation.stall_threshold_secs => WatchdogStatus::CatchingUp,
            // Behind AND (aged past the window OR age unknown) → fail closed to a stalled cursor.
            _ => WatchdogStatus::StalledCursor,
        },
    }
}

/// The read-only watchdog result for one corpus (the verdict + the facts behind it, for JSON/diagnostics).
#[derive(Debug, Clone, Serialize)]
pub struct CorpusWatchdogResult {
    pub corpus: String,
    pub status: WatchdogStatus,
    pub code: &'static str,
    pub applied_sequence: Option<u64>,
    pub producer_head_sequence: u64,
    pub cursor_age_secs: Option<u64>,
    pub stall_threshold_secs: u64,
}

impl CorpusWatchdogResult {
    #[must_use]
    pub fn from_observation(corpus: &str, observation: WatchdogObservation) -> Self {
        let status = classify_watchdog(observation);
        Self {
            corpus: corpus.to_owned(),
            status,
            code: status.code(),
            applied_sequence: observation.applied_sequence,
            producer_head_sequence: observation.producer_head_sequence,
            cursor_age_secs: observation.cursor_age_secs,
            stall_threshold_secs: observation.stall_threshold_secs,
        }
    }

    #[must_use]
    pub fn to_line(&self) -> String {
        let applied = self
            .applied_sequence
            .map_or_else(|| "none".to_owned(), |sequence| sequence.to_string());
        format!(
            "[{}] {}: applied={applied} head={} — {}",
            if self.status.is_alert() {
                "ALERT"
            } else {
                "OK"
            },
            self.corpus,
            self.producer_head_sequence,
            self.status.describe(),
        )
    }
}

/// Read-only watchdog for ONE corpus: read the applied cursor (+ its `applied_at` age) and the VERIFIED
/// producer head, then classify. Opens the writer connection ONLY for SELECTs (`corpus_status` /
/// `read_client_cursor`) and reads the package source for the signed manifest — it NEVER applies.
pub fn watchdog_corpus(
    conn: &dyn WriterConnection,
    config: &SiteConfig,
    corpus: &str,
    now_unix: u64,
    stall_threshold_secs: u64,
) -> Result<CorpusWatchdogResult, DeployError> {
    // Verified producer head (read-only fetch + signature/trust verification — no apply).
    let verifier =
        load_package_verifier(conn).map_err(|error| sync_err("watchdog.verifier", error))?;
    let source = DirectoryCatchupSource::new(&config.sync.source_root, DEFAULT_URI_BASE);
    let manifest = fetch_verify_manifest(&source, &verifier, corpus)
        .map_err(|error| sync_err("watchdog.manifest", error))?;
    let head = manifest.head_sequence.get();

    // Applied cursor sequence (control-plane position truth) and its last-advance timestamp.
    let cursor =
        read_client_cursor(conn, corpus).map_err(|error| sync_err("watchdog.cursor", error))?;
    let applied_sequence = cursor.as_ref().map(|cursor| cursor.sequence);

    let cursor_age_secs = applied_at_for(conn, corpus)?
        .as_deref()
        .and_then(parse_timestamptz_unix)
        .map(|applied_unix| now_unix.saturating_sub(applied_unix));

    let observation = WatchdogObservation {
        applied_sequence,
        producer_head_sequence: head,
        cursor_age_secs,
        stall_threshold_secs,
    };
    Ok(CorpusWatchdogResult::from_observation(corpus, observation))
}

/// The `applied_at` timestamp for a corpus from `corpus_status` (the same SELECT-only authority the
/// readiness path reads). `None` when the corpus is not present.
fn applied_at_for(
    conn: &dyn WriterConnection,
    corpus: &str,
) -> Result<Option<String>, DeployError> {
    let statuses = corpus_status(conn).map_err(|error| {
        let mut errors = crate::error::ValidationErrors::default();
        errors.push(
            "watchdog.status",
            error.to_string(),
            "check the site database connection",
        );
        DeployError::Validation(errors)
    })?;
    Ok(statuses
        .into_iter()
        .find(|status| status.corpus == corpus)
        .and_then(|status| status.applied_at))
}

fn sync_err(code: &'static str, error: jurisearch_syncd::SyncError) -> DeployError {
    let mut errors = crate::error::ValidationErrors::default();
    errors.push(
        code,
        error.to_string(),
        "check trust anchors, the package source root, and corpus entitlement",
    );
    DeployError::Validation(errors)
}

/// Parse a PostgreSQL `timestamptz::text` value (the ACTUAL form `corpus_status` returns for `applied_at`)
/// to whole UNIX seconds, dependency-free (Howard Hinnant's days-from-civil). The watchdog reads
/// `applied_at::text`, which PostgreSQL renders SPACE-separated with a numeric offset, e.g.
/// `2026-06-29 12:00:00+00` or `2026-06-29 12:00:00.123456+02:30`. The RFC3339 `T`-separated form
/// (`2026-06-29T12:00:00Z`) is also accepted. The trailing offset (`Z`, `+HH[:MM]`, `-HH[:MM]`, `+HHMM`)
/// is APPLIED so the resulting epoch is correct regardless of the session timezone PostgreSQL printed in;
/// anything unreadable falls back to `None` (which the behind-cursor decision treats as a stall).
#[must_use]
pub fn parse_timestamptz_unix(timestamp: &str) -> Option<u64> {
    // The date and the time are separated by an ISO `T` OR a PostgreSQL space.
    let (date, rest) = timestamp.trim().split_once(['T', ' '])?;
    // Peel the timezone offset off the END, leaving `HH:MM:SS[.fffff]`. Apply it as a UTC correction.
    let (time_part, offset_secs) = split_offset(rest)?;
    // Drop any fractional-second tail — whole-second resolution is all the stall age needs.
    let time = time_part.split('.').next()?;

    let mut d = date.split('-');
    let year: i64 = d.next()?.parse().ok()?;
    let month: i64 = d.next()?.parse().ok()?;
    let day: i64 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let hour: i64 = t.next()?.parse().ok()?;
    let minute: i64 = t.next()?.parse().ok()?;
    let second: i64 = t.next().unwrap_or("0").parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    // Local civil seconds, then subtract the offset to land on UTC (`+02:00` means 2h AHEAD of UTC).
    let secs = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_secs;
    u64::try_from(secs).ok()
}

/// Split the trailing timezone designator off the time portion, returning `(HH:MM:SS[.fff], offset_secs)`
/// where `offset_secs` is the seconds EAST of UTC (`+` positive, `-` negative, `Z`/`z` = 0). A naive value
/// with no designator is treated as UTC (`0`) so it still parses rather than failing closed prematurely.
fn split_offset(rest: &str) -> Option<(&str, i64)> {
    if let Some(stripped) = rest.strip_suffix(['Z', 'z']) {
        return Some((stripped, 0));
    }
    // The time itself is `HH:MM:SS` (no sign); the FIRST `+`/`-` after it begins the offset.
    if let Some(index) = rest.find(['+', '-']) {
        let (time, designator) = rest.split_at(index);
        let sign = if designator.starts_with('-') { -1 } else { 1 };
        let magnitude = parse_offset_magnitude(&designator[1..])?;
        return Some((time, sign * magnitude));
    }
    Some((rest, 0))
}

/// Parse an offset magnitude in seconds from `HH`, `HH:MM`, or `HHMM`.
fn parse_offset_magnitude(offset: &str) -> Option<i64> {
    let (hours_str, minutes_str) = if let Some((hours, minutes)) = offset.split_once(':') {
        (hours, minutes)
    } else if offset.len() == 4 {
        (&offset[..2], &offset[2..])
    } else {
        (offset, "0")
    };
    let hours: i64 = hours_str.parse().ok()?;
    let minutes: i64 = minutes_str.parse().ok()?;
    Some(hours * 3_600 + minutes * 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    fn obs(applied: Option<u64>, head: u64, age: Option<u64>) -> WatchdogObservation {
        WatchdogObservation {
            applied_sequence: applied,
            producer_head_sequence: head,
            cursor_age_secs: age,
            stall_threshold_secs: 2 * DAY,
        }
    }

    #[test]
    fn cursor_at_head_is_no_new_packages_not_a_stall() {
        // The crux of the gate: applied == head means the producer has no new packages — NOT a stall.
        let status = classify_watchdog(obs(Some(7), 7, Some(30 * DAY)));
        assert_eq!(status, WatchdogStatus::NoNewPackages);
        assert!(!status.is_alert());
    }

    #[test]
    fn cursor_behind_and_stale_is_a_stalled_cursor() {
        // applied < head AND the cursor has not advanced within the window → a stuck site sync cursor.
        let status = classify_watchdog(obs(Some(3), 7, Some(3 * DAY)));
        assert_eq!(status, WatchdogStatus::StalledCursor);
        assert!(status.is_alert());
    }

    #[test]
    fn behind_but_recently_advanced_is_a_normal_catch_up_not_a_stall() {
        let status = classify_watchdog(obs(Some(5), 7, Some(60)));
        assert_eq!(status, WatchdogStatus::CatchingUp);
        assert!(!status.is_alert());
    }

    #[test]
    fn behind_with_unknown_age_fails_closed_to_a_stalled_cursor() {
        // FAIL-CLOSED: a cursor BEHIND the verified head whose applied_at age is unknown
        // (missing/unparseable) must NEVER report the healthy CatchingUp state — it is a stall to alert on.
        let status = classify_watchdog(obs(Some(5), 7, None));
        assert_eq!(status, WatchdogStatus::StalledCursor);
        assert!(status.is_alert());
    }

    #[test]
    fn at_head_with_unknown_age_is_still_healthy_no_new_packages() {
        // Fail-closed applies ONLY to a behind cursor; a cursor AT the head is healthy regardless of age.
        assert_eq!(
            classify_watchdog(obs(Some(7), 7, None)),
            WatchdogStatus::NoNewPackages
        );
    }

    #[test]
    fn no_active_corpus_and_ahead_of_head_are_their_own_states() {
        assert_eq!(
            classify_watchdog(obs(None, 7, None)),
            WatchdogStatus::NoActiveCorpus
        );
        let ahead = classify_watchdog(obs(Some(9), 7, Some(10)));
        assert_eq!(ahead, WatchdogStatus::AheadOfHead);
        assert!(ahead.is_alert());
    }

    #[test]
    fn stalled_and_no_new_packages_are_distinct_codes() {
        // The two states the gate insists be DISTINGUISHED must have distinct, stable codes.
        assert_ne!(
            WatchdogStatus::StalledCursor.code(),
            WatchdogStatus::NoNewPackages.code()
        );
        assert_eq!(
            WatchdogStatus::StalledCursor.code(),
            "watchdog.stalled_cursor"
        );
        assert_eq!(
            WatchdogStatus::NoNewPackages.code(),
            "watchdog.no_new_packages"
        );
    }

    #[test]
    fn the_real_postgresql_applied_at_format_parses_to_unix_and_ages_correctly() {
        // The ACTUAL `applied_at::text` PostgreSQL renders: SPACE-separated, numeric `+00` offset. This is
        // the value that previously failed `split_once('T')` and produced the forbidden false-green.
        const EPOCH: u64 = 1_782_734_400; // 2026-06-29 12:00:00 UTC
        assert_eq!(
            parse_timestamptz_unix("2026-06-29 12:00:00+00"),
            Some(EPOCH)
        );
        // PostgreSQL with microsecond precision + `+00:00` offset.
        assert_eq!(
            parse_timestamptz_unix("2026-06-29 12:00:00.123456+00:00"),
            Some(EPOCH)
        );
        // The RFC3339 `T`-separated form is still accepted (both `Z` and a fractional suffix).
        assert_eq!(parse_timestamptz_unix("2026-06-29T12:00:00Z"), Some(EPOCH));
        assert_eq!(
            parse_timestamptz_unix("2026-06-29T12:00:00.500Z"),
            Some(EPOCH)
        );
        // Non-zero offsets are APPLIED: `14:00:00+02` and `08:30:00-03:30` are both 12:00:00 UTC.
        assert_eq!(
            parse_timestamptz_unix("2026-06-29 14:00:00+02"),
            Some(EPOCH)
        );
        assert_eq!(
            parse_timestamptz_unix("2026-06-29 08:30:00-03:30"),
            Some(EPOCH)
        );
        assert_eq!(parse_timestamptz_unix("not-a-time"), None);
    }

    #[test]
    fn a_behind_cursor_with_a_real_pg_applied_at_is_aged_and_classified_not_falsely_green() {
        // End-to-end of the BLOCKER fix: a real PG timestamp must yield a numeric age, and a behind cursor
        // aged past the window must classify as a stall — not a CatchingUp false-green.
        const APPLIED: u64 = 1_782_734_400; // 2026-06-29 12:00:00 UTC
        let parsed = parse_timestamptz_unix("2026-06-29 12:00:00+00")
            .expect("real PG applied_at must parse");
        let now = APPLIED + 5 * DAY; // five days behind → past the 2-day window
        let age = now.saturating_sub(parsed);
        let status = classify_watchdog(obs(Some(3), 7, Some(age)));
        assert_eq!(status, WatchdogStatus::StalledCursor);
        assert!(status.is_alert());
    }

    #[test]
    fn the_result_line_marks_only_alert_states() {
        let stalled =
            CorpusWatchdogResult::from_observation("core", obs(Some(3), 7, Some(5 * DAY)));
        assert!(stalled.to_line().contains("ALERT"));
        let healthy = CorpusWatchdogResult::from_observation("core", obs(Some(7), 7, None));
        assert!(healthy.to_line().contains("[OK]"));
        assert!(!healthy.to_line().contains("ALERT"));
    }
}
