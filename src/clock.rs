//! Minimal UTC timestamp handling.
//!
//! Only a few things are needed: stamp the synced document, and turn the
//! lifetime the server granted into the instant the paste will lapse so we can
//! warn before it does. That is small enough to do directly rather than pull in
//! a date crate — and the algorithms below are pinned by tests against
//! independently computed values.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current UTC time as `2026-09-20T02:36:04Z`.
pub fn now_rfc3339() -> String {
    format_epoch(now_epoch())
}

/// Seconds since the Unix epoch, saturating at 0 for a clock before 1970.
pub fn now_epoch() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        // A clock before 1970 is not worth failing over.
        Err(_) => 0,
    }
}

/// Format a Unix timestamp as RFC 3339 UTC, second precision.
pub fn format_epoch(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Parse the timestamps the pastebin emits, e.g. `2025-05-08T10:33:06.114Z`.
/// Fractional seconds and a trailing `Z` are accepted and ignored.
pub fn parse_rfc3339_epoch(s: &str) -> Option<i64> {
    let s = s.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { s.get(from..to)?.parse::<i64>().ok() };
    let y = num(0, 4)?;
    let m = num(5, 7)?;
    let d = num(8, 10)?;
    let hh = num(11, 13)?;
    let mm = num(14, 16)?;
    let ss = num(17, 19)?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m as u32, d as u32) * 86_400 + hh * 3600 + mm * 60 + ss)
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Inverse of [`civil_from_days`].
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expected values computed independently with Python's `datetime`.
    #[test]
    fn formats_known_timestamps() {
        let cases = [
            (0, "1970-01-01T00:00:00Z"),
            (1_000_000_000, "2001-09-09T01:46:40Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (1_758_326_400, "2025-09-20T00:00:00Z"),
            (4_102_444_800, "2100-01-01T00:00:00Z"),
            (1_758_326_400 + 3661, "2025-09-20T01:01:01Z"),
        ];
        for (epoch, expected) in cases {
            assert_eq!(format_epoch(epoch), expected, "epoch {epoch}");
        }
    }

    /// 2000 was a leap year (divisible by 400); 2100 is not (divisible by 100
    /// but not 400). Both are covered above; this pins the day arithmetic.
    #[test]
    fn handles_leap_years_and_centuries() {
        assert_eq!(days_from_civil(2000, 2, 29), 11016);
        assert_eq!(civil_from_days(11016), (2000, 2, 29));
        // 2100-03-01 must be exactly one day after 2100-02-28: no Feb 29.
        let feb28 = days_from_civil(2100, 2, 28);
        assert_eq!(days_from_civil(2100, 3, 1), feb28 + 1);
    }

    #[test]
    fn parses_what_the_server_sends() {
        assert_eq!(parse_rfc3339_epoch("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_epoch("2025-05-08T10:33:06.114Z"),
            Some(1_746_700_386)
        );
        assert_eq!(
            parse_rfc3339_epoch("2025-05-08T10:33:06Z"),
            Some(1_746_700_386)
        );
    }

    #[test]
    fn format_and_parse_round_trip() {
        for epoch in [0, 1, 951_782_400, 1_758_326_400, 4_102_444_800] {
            let s = format_epoch(epoch);
            assert_eq!(parse_rfc3339_epoch(&s), Some(epoch), "round trip {s}");
        }
    }

    #[test]
    fn rejects_malformed_input() {
        for s in ["", "not-a-date", "2025-13-01T00:00:00Z", "2025-05-08"] {
            assert_eq!(parse_rfc3339_epoch(s), None, "{s} should not parse");
        }
    }

    #[test]
    fn now_looks_like_a_timestamp() {
        let now = now_rfc3339();
        assert_eq!(now.len(), 20);
        assert!(parse_rfc3339_epoch(&now).unwrap() > 1_600_000_000);
    }
}
