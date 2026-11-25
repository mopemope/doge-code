//! UTC timestamps in RFC3339 form without a date/time dependency.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current UTC time as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub fn now_rfc3339() -> String {
    // A clock before 1970 is treated as the epoch rather than failing.
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format_rfc3339(millis)
}

/// Format milliseconds since the Unix epoch as `YYYY-MM-DDTHH:MM:SS.mmmZ` (UTC).
pub fn format_rfc3339(unix_millis: u128) -> String {
    let millis = (unix_millis % 1000) as u32;
    let total_secs = unix_millis / 1000;
    let days = (total_secs / 86_400) as i64;
    let secs_of_day = (total_secs % 86_400) as u32;
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    );
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day).
/// Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn leap_day_with_millis() {
        assert_eq!(format_rfc3339(951_782_400_123), "2000-02-29T00:00:00.123Z");
    }

    #[test]
    fn after_2038() {
        // 2^31 seconds: the 32-bit time_t rollover.
        assert_eq!(
            format_rfc3339(2_147_483_648_123),
            "2038-01-19T03:14:08.123Z"
        );
        assert_eq!(
            format_rfc3339(4_102_444_800_000),
            "2100-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn end_of_day_and_year() {
        assert_eq!(
            format_rfc3339(1_798_761_599_999),
            "2026-12-31T23:59:59.999Z"
        );
    }

    #[test]
    fn now_has_rfc3339_shape() {
        let s = now_rfc3339();
        assert_eq!(s.len(), 24);
        assert!(s.ends_with('Z'));
        assert_eq!(&s[10..11], "T");
    }
}
