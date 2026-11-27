//! Five-field cron expressions evaluated in the daemon's local time.

use super::localtime::{LocalClock, LocalDateTime};
use std::ops::RangeInclusive;

#[derive(Debug, thiserror::Error)]
pub enum CronError {
    #[error("a cron expression needs five fields or a supported macro")]
    FieldCount,
    #[error("invalid cron field {position} ({name})")]
    InvalidField { position: usize, name: &'static str },
}

#[derive(Debug, Clone)]
pub struct CronExpr {
    minutes: Values,
    hours: Values,
    days: Values,
    months: Values,
    weekdays: Values,
    day_wildcard: bool,
    weekday_wildcard: bool,
}

impl CronExpr {
    pub fn parse(s: &str) -> Result<Self, CronError> {
        let expression = match s.trim() {
            "@hourly" => "0 * * * *",
            "@daily" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            expression => expression,
        };
        let fields: Vec<_> = expression.split_whitespace().take(6).collect();
        let [minute, hour, day, month, weekday] = fields[..] else {
            return Err(CronError::FieldCount);
        };

        let minutes = parse_field(minute, 1, "minute", 0..=59, &[])?;
        let hours = parse_field(hour, 2, "hour", 0..=23, &[])?;
        let days = parse_field(day, 3, "day of month", 1..=31, &[])?;
        let months = parse_field(
            month,
            4,
            "month",
            1..=12,
            &[
                "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
            ],
        )?;
        let mut weekdays = parse_field(
            weekday,
            5,
            "day of week",
            0..=7,
            &["sun", "mon", "tue", "wed", "thu", "fri", "sat"],
        )?;
        if weekdays.contains(7) {
            weekdays.0 = (weekdays.0 & !(1 << 7)) | 1;
        }

        Ok(Self {
            minutes,
            hours,
            days,
            months,
            weekdays,
            // Vixie cron sets the day flags from each field's first character.
            day_wildcard: day.starts_with('*'),
            weekday_wildcard: weekday.starts_with('*'),
        })
    }

    /// Find the next matching local minute within five calendar years.
    pub fn next_after(&self, unix: i64, clock: &dyn LocalClock) -> Option<i64> {
        let local = clock.to_local(unix);
        let mut date = Date::from_local(local)?;
        let cutoff = date.five_years_later();
        let last_time = (local.hour, local.minute);
        while cutoff.is_none_or(|cutoff| date <= cutoff) {
            if self.matches_date(date) {
                for hour in self.hours.iter() {
                    for minute in self.minutes.iter() {
                        if Some(date) == cutoff && (hour, minute) > last_time {
                            return None;
                        }
                        // During a fold, an earlier wall minute may still be in the future.
                        if let Some(candidate) =
                            clock.from_local(date.year, date.month, date.day, hour, minute)
                            && candidate > unix
                        {
                            return Some(candidate);
                        }
                    }
                }
            }
            date = date.next()?;
        }
        None
    }

    fn matches_date(&self, date: Date) -> bool {
        if !self.months.contains(date.month) {
            return false;
        }
        let day_matches = self.days.contains(date.day);
        let weekday_matches = self.weekdays.contains(date.weekday());
        if self.day_wildcard || self.weekday_wildcard {
            day_matches && weekday_matches
        } else {
            day_matches || weekday_matches
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Values(u64);

impl Values {
    fn contains(self, value: u32) -> bool {
        self.0 & (1 << value) != 0
    }

    fn iter(self) -> impl Iterator<Item = u32> {
        let mut bits = self.0;
        std::iter::from_fn(move || {
            let value = bits.trailing_zeros();
            bits &= bits.checked_sub(1)?;
            Some(value)
        })
    }
}

fn parse_field(
    text: &str,
    position: usize,
    name: &'static str,
    bounds: RangeInclusive<u32>,
    names: &[&str],
) -> Result<Values, CronError> {
    let value = |text: &str| {
        let value = parse_decimal(text).or_else(|| {
            let index = names
                .iter()
                .position(|name| name.eq_ignore_ascii_case(text))?;
            Some(*bounds.start() + index as u32)
        })?;
        bounds.contains(&value).then_some(value)
    };

    let parse_item = |item: &str| {
        let (range, step) = match item.split_once('/') {
            Some((range, step)) => (range, parse_decimal(step).filter(|&n| n > 0)?),
            None => (item, 1),
        };
        let (start, end) = if range == "*" {
            (*bounds.start(), *bounds.end())
        } else if let Some((start, end)) = range.split_once('-') {
            (value(start)?, value(end)?)
        } else if range == item {
            let single = value(range)?;
            (single, single)
        } else {
            return None;
        };
        if start > end {
            return None;
        }
        Some(
            (start..=end)
                .step_by(step as usize)
                .fold(0, |bits, n| bits | (1 << n)),
        )
    };

    let mut bits = 0;
    for item in text.split(',') {
        bits |= parse_item(item).ok_or(CronError::InvalidField { position, name })?;
    }
    Ok(Values(bits))
}

fn parse_decimal(text: &str) -> Option<u32> {
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Date {
    year: i32,
    month: u32,
    day: u32,
}

impl Date {
    fn from_local(local: LocalDateTime) -> Option<Self> {
        let LocalDateTime {
            year,
            month,
            day,
            hour,
            minute,
            ..
        } = local;
        ((1..=days_in_month(year, month)).contains(&day) && hour < 24 && minute < 60)
            .then_some(Self { year, month, day })
    }

    fn five_years_later(self) -> Option<Self> {
        let year = self.year.checked_add(5)?;
        Some(Self {
            year,
            day: self.day.min(days_in_month(year, self.month)),
            ..self
        })
    }

    fn next(self) -> Option<Self> {
        if self.day < days_in_month(self.year, self.month) {
            Some(Self {
                day: self.day + 1,
                ..self
            })
        } else if self.month < 12 {
            Some(Self {
                month: self.month + 1,
                day: 1,
                ..self
            })
        } else {
            Some(Self {
                year: self.year.checked_add(1)?,
                month: 1,
                day: 1,
            })
        }
    }

    fn weekday(self) -> u32 {
        // 0001-01-01 was Monday in the proleptic Gregorian calendar.
        let y = i64::from(self.year) - 1;
        let before_year = 365 * y + y.div_euclid(4) - y.div_euclid(100) + y.div_euclid(400);
        let day_of_year = (1..self.month)
            .map(|mo| days_in_month(self.year, mo))
            .sum::<u32>()
            + self.day;
        (before_year + i64::from(day_of_year)).rem_euclid(7) as u32
    }
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::localtime::tests::{FixedOffsetClock, utc};

    #[test]
    fn parse_accepts_fields_ranges_steps_lists_names_and_macros() {
        let clock = FixedOffsetClock(0);
        let start = utc(2026, 1, 12, 8, 14);
        for (name, fields) in [
            ("@hourly", "0 * * * *"),
            ("@daily", "0 0 * * *"),
            ("@weekly", "0 0 * * 0"),
            ("@monthly", "0 0 1 * *"),
            ("@yearly", "0 0 1 1 *"),
            ("@annually", "0 0 1 1 *"),
        ] {
            let expanded = CronExpr::parse(fields).expect("valid expanded expression");
            let named = CronExpr::parse(name).expect("valid cron macro");
            assert_eq!(
                named.next_after(start, &clock),
                expanded.next_after(start, &clock),
                "{name}"
            );
        }

        let expr =
            CronExpr::parse("  */15 8-12/2 1,15 jAn-MaR MoN,FRI\t").expect("valid combined fields");
        assert_eq!(
            expr.next_after(start, &clock),
            Some(utc(2026, 1, 12, 8, 15))
        );
        assert_eq!(
            expr.next_after(utc(2026, 1, 12, 8, 45), &clock),
            Some(utc(2026, 1, 12, 10, 0))
        );
        assert_eq!(
            expr.next_after(utc(2026, 4, 1, 0, 0), &clock),
            Some(utc(2027, 1, 1, 8, 0))
        );
    }

    #[test]
    fn parse_rejects_out_of_range_and_bad_syntax() {
        for (input, details) in [
            ("60 * * * *", "1 (minute)"),
            ("* * 0 * *", "3 (day of month)"),
            ("*/0 * * * *", "1 (minute)"),
            ("* * * 13 *", "4 (month)"),
            ("* * * * * *", "five fields"),
            ("* * * *", "five fields"),
            ("* 10-5 * * *", "2 (hour)"),
            ("0, * * * *", "1 (minute)"),
            ("10/5 * * * *", "1 (minute)"),
            ("+5 * * * *", "1 (minute)"),
            ("* * * * private-fragment", "5 (day of week)"),
            ("@reboot", "five fields"),
        ] {
            let error = CronExpr::parse(input).expect_err("invalid cron expression");
            assert!(error.to_string().contains(details), "{error}");
            assert!(!error.to_string().contains(input));
            assert!(!error.to_string().contains("private-fragment"));
        }
    }

    #[test]
    fn next_after_every_minute_and_hourly() {
        let clock = FixedOffsetClock(9 * 3600);
        let every_minute = CronExpr::parse("* * * * *").expect("every minute");
        let hourly = CronExpr::parse("@hourly").expect("hourly");
        let start = utc(2026, 11, 27, 14, 59);

        for unix in [start, start + 59] {
            assert_eq!(every_minute.next_after(unix, &clock), Some(start + 60));
            assert_eq!(hourly.next_after(unix, &clock), Some(start + 60));
        }
        assert_eq!(hourly.next_after(start + 60, &clock), Some(start + 3660));
        assert_eq!(every_minute.next_after(-1, &clock), Some(0));
    }

    #[test]
    fn dom_and_dow_restricted_matches_either() {
        let clock = FixedOffsetClock(0);
        let expr = CronExpr::parse("0 0 1 * MON").expect("day of month or Monday");
        let day_of_month = utc(2026, 11, 1, 0, 0);
        let weekday = utc(2026, 11, 2, 0, 0);
        assert_eq!(
            expr.next_after(day_of_month - 1, &clock),
            Some(day_of_month)
        );
        assert_eq!(expr.next_after(day_of_month, &clock), Some(weekday));
        assert_eq!(
            expr.next_after(weekday, &clock),
            Some(utc(2026, 11, 9, 0, 0))
        );
    }

    #[test]
    fn dom_step_star_is_wildcard_and_combines_with_dow() {
        let clock = FixedOffsetClock(0);
        let expr = CronExpr::parse("0 9 */2 * 1-5").expect("odd-numbered weekdays");
        let tuesday = utc(2026, 11, 3, 9, 0);
        let thursday = utc(2026, 11, 5, 9, 0);
        assert_eq!(
            expr.next_after(utc(2026, 11, 1, 8, 59), &clock),
            Some(tuesday)
        );
        assert_eq!(expr.next_after(tuesday, &clock), Some(thursday));
        assert_eq!(
            expr.next_after(thursday, &clock),
            Some(utc(2026, 11, 9, 9, 0))
        );
    }

    #[test]
    fn dow_step_star_is_wildcard_and_combines_with_dom() {
        let clock = FixedOffsetClock(0);
        let expr = CronExpr::parse("0 9 1,2 * */2").expect("first or second on even weekdays");
        let sunday = utc(2026, 11, 1, 9, 0);
        assert_eq!(expr.next_after(sunday - 1, &clock), Some(sunday));
        assert_eq!(
            expr.next_after(sunday, &clock),
            Some(utc(2026, 12, 1, 9, 0))
        );
    }

    #[test]
    fn dow_seven_is_sunday() {
        let clock = FixedOffsetClock(0);
        let sunday = utc(2026, 11, 29, 0, 0);
        for dow in ["7", "0", "sUn", "5-7", "5-7/2"] {
            let expr = CronExpr::parse(&format!("0 0 * * {dow}")).expect("includes Sunday");
            assert_eq!(expr.next_after(sunday - 1, &clock), Some(sunday), "{dow}");
        }
    }

    #[test]
    fn never_firing_expression_returns_none() {
        let clock = FixedOffsetClock(0);
        let impossible = CronExpr::parse("0 0 31 2 *").expect("syntactically valid");
        assert_eq!(impossible.next_after(utc(2026, 11, 28, 0, 0), &clock), None);

        let leap_day = CronExpr::parse("0 0 29 2 *").expect("leap days");
        assert_eq!(
            leap_day.next_after(utc(2026, 11, 28, 0, 0), &clock),
            Some(utc(2028, 2, 29, 0, 0))
        );
        assert_eq!(leap_day.next_after(utc(2096, 2, 29, 0, 0), &clock), None);
    }

    struct DstClock;

    impl LocalClock for DstClock {
        fn to_local(&self, unix: i64) -> LocalDateTime {
            let spring = utc(2026, 3, 29, 2, 0);
            let fall = utc(2026, 10, 25, 2, 0);
            let offset = if (spring..fall).contains(&unix) {
                3600
            } else {
                0
            };
            FixedOffsetClock(offset).to_local(unix)
        }

        fn from_local(&self, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> Option<i64> {
            // Prefer standard time when both offsets map to this wall minute.
            [0, 3600].into_iter().find_map(|offset| {
                let clock = FixedOffsetClock(offset);
                let unix = clock.from_local(y, mo, d, h, mi)?;
                (clock.to_local(unix) == self.to_local(unix)).then_some(unix)
            })
        }
    }

    #[test]
    fn nonexistent_local_time_is_skipped_and_repeated_time_fires_once() {
        let expr = CronExpr::parse("30 2 * * *").expect("daily at 02:30");
        assert_eq!(
            expr.next_after(utc(2026, 3, 29, 0, 0), &DstClock),
            Some(utc(2026, 3, 30, 1, 30))
        );

        let repeated = utc(2026, 10, 25, 2, 30);
        assert_eq!(
            expr.next_after(utc(2026, 10, 25, 0, 0), &DstClock),
            Some(repeated)
        );
        assert_eq!(
            expr.next_after(utc(2026, 10, 25, 1, 50), &DstClock),
            Some(repeated)
        );
        assert_eq!(
            expr.next_after(repeated, &DstClock),
            Some(utc(2026, 10, 26, 2, 30))
        );
    }
}
