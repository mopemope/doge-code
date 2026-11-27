//! Conversion between Unix seconds and the daemon's local wall time.

use std::mem::MaybeUninit;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalDateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub offset_secs: i32,
}

const UNREPRESENTABLE: LocalDateTime = LocalDateTime {
    year: 0,
    month: 0,
    day: 0,
    hour: 0,
    minute: 0,
    offset_secs: 0,
};

pub trait LocalClock: Send + Sync {
    /// Return the local date and minute. An unrepresentable timestamp has month 0.
    fn to_local(&self, unix: i64) -> LocalDateTime;

    /// Return the instant at second zero of this wall minute.
    /// A skipped minute has no instant and a repeated minute has one canonical instant.
    #[allow(clippy::wrong_self_convention)]
    fn from_local(&self, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> Option<i64>;
}

pub struct SystemClock;

// glibc guesses an offset from the last mktime call, which can otherwise
// change the selected side of a fold between successive scheduler checks.
static MKTIME_LOCK: Mutex<()> = Mutex::new(());

impl LocalClock for SystemClock {
    fn to_local(&self, unix: i64) -> LocalDateTime {
        checked_local(unix).unwrap_or(UNREPRESENTABLE)
    }

    fn from_local(&self, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> Option<i64> {
        if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h >= 24 || mi >= 60 {
            return None;
        }
        let mut requested = wall_time(
            y.checked_sub(1900)?,
            mo as i32 - 1,
            d as i32,
            h as i32,
            mi as i32,
        );
        let unix = {
            let _lock = MKTIME_LOCK.lock().ok()?;
            // Seed the offset guess with 1970-01-02 at noon, within 32-bit time_t.
            // Then let mktime with tm_isdst=-1 choose the canonical instant.
            mktime(&mut wall_time(70, 0, 2, 12, 0))?;
            mktime(&mut requested)?
        };
        let actual = checked_local(unix)?;
        ((
            actual.year,
            actual.month,
            actual.day,
            actual.hour,
            actual.minute,
            requested.tm_sec,
        ) == (y, mo, d, h, mi, 0))
            .then_some(unix)
    }
}

fn wall_time(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> libc::tm {
    libc::tm {
        tm_year: year,
        tm_mon: month,
        tm_mday: day,
        tm_hour: hour,
        tm_min: minute,
        tm_sec: 0,
        tm_wday: -1,
        tm_yday: 0,
        tm_isdst: -1,
        tm_gmtoff: 0,
        tm_zone: Default::default(),
    }
}

fn mktime(tm: &mut libc::tm) -> Option<i64> {
    // SAFETY: tm is initialized and uniquely borrowed for the duration of the call.
    let unix = unsafe { libc::mktime(tm) };
    // -1 is also a valid Unix second. A successful call sets tm_wday.
    #[allow(clippy::useless_conversion)] // time_t can be narrower than i64.
    let unix = i64::from(unix);
    (tm.tm_wday >= 0).then_some(unix)
}

fn checked_local(unix: i64) -> Option<LocalDateTime> {
    #[allow(clippy::useless_conversion)] // time_t can be narrower than i64.
    let unix: libc::time_t = unix.try_into().ok()?;
    let mut tm = MaybeUninit::<libc::tm>::uninit();
    // SAFETY: unix is initialized and tm has space for the result.
    if unsafe { libc::localtime_r(&unix, tm.as_mut_ptr()) }.is_null() {
        return None;
    }
    // SAFETY: localtime_r initialized tm on success.
    let tm = unsafe { tm.assume_init() };
    Some(LocalDateTime {
        year: tm.tm_year.checked_add(1900)?,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        offset_secs: tm.tm_gmtoff.try_into().ok()?,
    })
}

/// Format a Unix timestamp as `YYYY-MM-DD HH:MM ±hhmm` in the daemon's timezone.
pub fn format_local(unix: i64) -> String {
    let Some(LocalDateTime {
        year,
        month,
        day,
        hour,
        minute,
        offset_secs,
    }) = checked_local(unix)
    else {
        return format!("unrepresentable local time ({unix})");
    };
    let sign = if offset_secs < 0 { '-' } else { '+' };
    let offset_minutes = offset_secs.unsigned_abs() / 60;
    let offset_hours = offset_minutes / 60;
    let offset_minutes = offset_minutes % 60;
    format!(
        "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02} {sign}{offset_hours:02}{offset_minutes:02}"
    )
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    pub(in crate::schedule) struct FixedOffsetClock(pub i32);

    impl LocalClock for FixedOffsetClock {
        fn to_local(&self, unix: i64) -> LocalDateTime {
            let shifted = (unix + i64::from(self.0)) as libc::time_t;
            let mut tm = MaybeUninit::<libc::tm>::uninit();
            // SAFETY: shifted is initialized and tm has space for the result.
            let result = unsafe { libc::gmtime_r(&shifted, tm.as_mut_ptr()) };
            assert!(!result.is_null());
            // SAFETY: gmtime_r initialized tm on success.
            let tm = unsafe { tm.assume_init() };
            LocalDateTime {
                year: tm.tm_year + 1900,
                month: (tm.tm_mon + 1) as u32,
                day: tm.tm_mday as u32,
                hour: tm.tm_hour as u32,
                minute: tm.tm_min as u32,
                offset_secs: self.0,
            }
        }

        fn from_local(&self, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> Option<i64> {
            Some(utc(y, mo, d, h, mi) - i64::from(self.0))
        }
    }

    pub(in crate::schedule) fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        // SAFETY: an all-zero tm contains valid integers and a null pointer.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        tm.tm_year = y - 1900;
        tm.tm_mon = mo as i32 - 1;
        tm.tm_mday = d as i32;
        tm.tm_hour = h as i32;
        tm.tm_min = mi as i32;
        // SAFETY: tm is initialized and uniquely borrowed for the duration of the call.
        #[allow(clippy::useless_conversion)] // time_t can be narrower than i64.
        i64::from(unsafe { libc::timegm(&mut tm) })
    }

    #[test]
    fn fixed_offset_clock_round_trips() {
        let clock = FixedOffsetClock(9 * 3600);
        let unix = utc(2026, 11, 28, 0, 0);
        assert_eq!(
            clock.to_local(unix),
            LocalDateTime {
                year: 2026,
                month: 11,
                day: 28,
                hour: 9,
                minute: 0,
                offset_secs: 32_400,
            }
        );
        assert_eq!(clock.from_local(2026, 11, 28, 9, 0), Some(unix));
    }

    #[test]
    fn system_clock_round_trips_current_time() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time since the epoch")
            .as_secs() as i64;
        let local = SystemClock.to_local(now);
        let unix = SystemClock
            .from_local(local.year, local.month, local.day, local.hour, local.minute)
            .expect("current wall minute exists");
        let round_trip = SystemClock.to_local(unix);

        // During a fold the same wall minute can have a different UTC offset.
        let offset_change = i64::from(local.offset_secs) - i64::from(round_trip.offset_secs);
        let seconds = (now + i64::from(local.offset_secs)).rem_euclid(60);
        assert_eq!(unix, now - seconds + offset_change);
        assert_eq!(
            round_trip,
            LocalDateTime {
                offset_secs: round_trip.offset_secs,
                ..local
            }
        );
        assert_eq!(SystemClock.from_local(2026, 2, 30, 9, 0), None);
    }

    #[test]
    fn system_clock_selects_same_repeated_time_after_other_conversions() {
        const CHILD: &str = "DGC_TEST_SYSTEM_CLOCK_DST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // Set TZ only in the child so parallel tests retain the original environment.
            let output = Command::new(std::env::current_exe().expect("unit test executable"))
                .args([
                    "--exact",
                    "schedule::localtime::tests::system_clock_selects_same_repeated_time_after_other_conversions",
                ])
                .env(CHILD, "1")
                .env("TZ", "EST5EDT,M3.2.0,M11.1.0")
                .output()
                .expect("run test with controlled DST rules");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "{stdout}"
            );
            return;
        }

        assert_eq!(SystemClock.from_local(2026, 3, 8, 2, 30), None);
        let summer = SystemClock
            .from_local(2026, 7, 1, 12, 0)
            .expect("summer noon");
        let first = SystemClock
            .from_local(2026, 11, 1, 1, 30)
            .expect("repeated minute");
        let winter = SystemClock
            .from_local(2026, 1, 1, 12, 0)
            .expect("winter noon");
        let second = SystemClock
            .from_local(2026, 11, 1, 1, 30)
            .expect("repeated minute");
        assert_eq!(format_local(summer), "2026-07-01 12:00 -0400");
        assert_eq!(format_local(winter), "2026-01-01 12:00 -0500");
        assert!([utc(2026, 11, 1, 5, 30), utc(2026, 11, 1, 6, 30)].contains(&first));
        assert_eq!(first, second);
    }
}
