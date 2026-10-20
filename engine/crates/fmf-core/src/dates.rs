//! Civil-date ↔ FILETIME conversion for `dm:` filters.
//!
//! `dm:` bounds are interpreted in the *local* time zone — a user typing a calendar date means their own day boundaries, not UTC's.
//! The runtime's date resolver owns time-zone conversion.

/// FILETIME ticks (100 ns since 1601-01-01) at the Unix epoch (1970-01-01).
pub const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
/// FILETIME ticks in one second (100 ns units).
pub const TICKS_PER_SECOND: i64 = 10_000_000;

const MONTHS_PER_YEAR: u32 = 12;
const FEBRUARY: u32 = 2;
const MARCH: u32 = 3;
const APRIL: u32 = 4;
const MAY: u32 = 5;
const JUNE: u32 = 6;
const JULY: u32 = 7;
const AUGUST: u32 = 8;
const SEPTEMBER: u32 = 9;
const OCTOBER: u32 = 10;
const NOVEMBER: u32 = 11;
const DECEMBER: u32 = 12;
const MIN_FILETIME_YEAR: i32 = 1601;
const MAX_FILETIME_YEAR: i32 = 9999;
const LONG_MONTH_DAYS: u32 = 31;
const SHORT_MONTH_DAYS: u32 = 30;
const LEAP_FEBRUARY_DAYS: u32 = 29;
const ORDINARY_FEBRUARY_DAYS: u32 = 28;
const LEAP_YEAR_INTERVAL: i32 = 4;
const YEARS_PER_CENTURY: i32 = 100;
const YEARS_PER_ERA: i64 = 400;
const PREVIOUS_ERA_ADJUSTMENT: i64 = YEARS_PER_ERA - 1;
const MARCH_MONTH_SHIFT: i64 = 3;
const WINTER_MONTH_SHIFT: i64 = 9;
const WINTER_MONTH_START: i64 = 10;
const MARCH_CYCLE_DAYS: i64 = 153;
const MONTH_DIVISION_BIAS: i64 = 2;
const MONTH_DIVISOR: i64 = 5;
const DAYS_PER_YEAR: i64 = 365;
const DAYS_PER_ERA: i64 = 146_097;
const LAST_DAY_OF_ERA: i64 = DAYS_PER_ERA - 1;
const FOUR_YEAR_CORRECTION: i64 = 1460;
const CENTURY_CORRECTION: i64 = 36_524;
const UNIX_EPOCH_MARCH_OFFSET: i64 = 719_468;
const SECONDS_PER_DAY: i64 = 86_400;

/// Encode a FILETIME tick count (100 ns since 1601) as the `mtime` column's stored form — Unix-epoch **seconds** in a `u32` (ADR-0031, −4 B/entry).
///
/// The map is monotonic, so it preserves mtime sort order; it drops sub-second precision, which `dm:` filters never observe (their bounds are day-aligned).
/// `0` is a reserved "unknown timestamp" sentinel: a 0 tick (a failed stat fetch) and every pre-1970 tick collapse to it, and [`mtime_secs_to_ticks`] maps it back to a 0 tick — so the FILETIME-0 "unknown" value round-trips unchanged.
/// Post-2106 saturates at `u32::MAX`.
#[must_use]
pub fn mtime_ticks_to_secs(ticks: i64) -> u32 {
    let secs = ticks.saturating_sub(FILETIME_UNIX_EPOCH) / TICKS_PER_SECOND;
    u32::try_from(secs).unwrap_or(if secs < 0 { 0 } else { u32::MAX })
}

/// Inverse of [`mtime_ticks_to_secs`], reconstructing a FILETIME tick count to the second.
/// The sentinel `0` maps back to a 0 tick (not the epoch).
#[must_use]
pub fn mtime_secs_to_ticks(secs: u32) -> i64 {
    if secs == 0 {
        0
    } else {
        FILETIME_UNIX_EPOCH + i64::from(secs) * TICKS_PER_SECOND
    }
}

/// A proleptic Gregorian calendar date with no time-of-day or zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Civil {
    /// Year (full, e.g. 2026).
    pub y: i32,
    /// Month, 1..=12.
    pub m: u32,
    /// Day of month, 1..=31.
    pub d: u32,
}

impl Civil {
    /// The calendar date one day after this one (handles month/leap rollover).
    #[must_use]
    pub const fn next_day(self) -> Self {
        civil_from_days(days_from_civil(self) + 1)
    }

    /// The first day of the month following this date's month.
    #[must_use]
    pub const fn first_of_next_month(self) -> Self {
        if self.m == DECEMBER {
            Self {
                y: self.y + 1,
                m: 1,
                d: 1,
            }
        } else {
            Self {
                y: self.y,
                m: self.m + 1,
                d: 1,
            }
        }
    }

    /// True if the date is a real calendar date within FILETIME's range (year 1601..=9999, valid month, day within the month's length).
    #[must_use]
    pub fn is_valid(self) -> bool {
        if !(MIN_FILETIME_YEAR..=MAX_FILETIME_YEAR).contains(&self.y)
            || !(1..=MONTHS_PER_YEAR).contains(&self.m)
        {
            return false;
        }
        self.d >= 1 && self.d <= days_in_month(self.y, self.m)
    }
}

const fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | MARCH | MAY | JULY | AUGUST | OCTOBER | DECEMBER => LONG_MONTH_DAYS,
        APRIL | JUNE | SEPTEMBER | NOVEMBER => SHORT_MONTH_DAYS,
        FEBRUARY => {
            if (y % LEAP_YEAR_INTERVAL == 0 && y % YEARS_PER_CENTURY != 0)
                || y % YEARS_PER_ERA as i32 == 0
            {
                LEAP_FEBRUARY_DAYS
            } else {
                ORDINARY_FEBRUARY_DAYS
            }
        }
        _ => 0,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
#[must_use]
pub const fn days_from_civil(c: Civil) -> i64 {
    let y = if c.m <= FEBRUARY { c.y - 1 } else { c.y } as i64;
    let era = if y >= 0 {
        y
    } else {
        y - PREVIOUS_ERA_ADJUSTMENT
    } / YEARS_PER_ERA;
    let yoe = y - era * YEARS_PER_ERA;
    let mp = (c.m as i64 + WINTER_MONTH_SHIFT) % MONTHS_PER_YEAR as i64;
    let doy = (MARCH_CYCLE_DAYS * mp + MONTH_DIVISION_BIAS) / MONTH_DIVISOR + c.d as i64 - 1;
    let doe = yoe * DAYS_PER_YEAR + yoe / LEAP_YEAR_INTERVAL as i64
        - yoe / YEARS_PER_CENTURY as i64
        + doy;
    era * DAYS_PER_ERA + doe - UNIX_EPOCH_MARCH_OFFSET
}

/// Inverse of [`days_from_civil`]: the civil date for a day count since 1970-01-01 (Howard Hinnant's `civil_from_days`).
#[must_use]
pub const fn civil_from_days(days: i64) -> Civil {
    let z = days + UNIX_EPOCH_MARCH_OFFSET;
    let era = if z >= 0 { z } else { z - LAST_DAY_OF_ERA } / DAYS_PER_ERA;
    let doe = z - era * DAYS_PER_ERA;
    let yoe = (doe - doe / FOUR_YEAR_CORRECTION + doe / CENTURY_CORRECTION - doe / LAST_DAY_OF_ERA)
        / DAYS_PER_YEAR;
    let y = yoe + era * YEARS_PER_ERA;
    let doy = doe
        - (DAYS_PER_YEAR * yoe + yoe / LEAP_YEAR_INTERVAL as i64 - yoe / YEARS_PER_CENTURY as i64);
    let mp = (MONTH_DIVISOR * doy + MONTH_DIVISION_BIAS) / MARCH_CYCLE_DAYS;
    let d = (doy - (MARCH_CYCLE_DAYS * mp + MONTH_DIVISION_BIAS) / MONTH_DIVISOR + 1) as u32;
    let m = if mp < WINTER_MONTH_START {
        mp + MARCH_MONTH_SHIFT
    } else {
        mp - WINTER_MONTH_SHIFT
    } as u32;
    Civil {
        y: (if m <= FEBRUARY { y + 1 } else { y }) as i32,
        m,
        d,
    }
}

/// FILETIME ticks for midnight in UTC.
#[must_use]
pub const fn utc_midnight(c: Civil) -> i64 {
    FILETIME_UNIX_EPOCH + days_from_civil(c) * SECONDS_PER_DAY * TICKS_PER_SECOND
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days_roundtrip() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2026, 6, 10), (1999, 12, 31)] {
            let c = Civil { y, m, d };
            assert_eq!(civil_from_days(days_from_civil(c)), c);
        }
        assert_eq!(
            days_from_civil(Civil {
                y: 1970,
                m: 1,
                d: 1
            }),
            0
        );
    }

    #[test]
    fn mtime_secs_codec_roundtrips_and_clamps() {
        let ft = 132_539_040_000_000_000i64;
        assert_eq!(mtime_secs_to_ticks(mtime_ticks_to_secs(ft)), ft);
        assert_eq!(mtime_secs_to_ticks(mtime_ticks_to_secs(ft + 9_999_999)), ft);
        assert_eq!(mtime_ticks_to_secs(0), 0);
        assert_eq!(mtime_secs_to_ticks(0), 0);
        assert_eq!(mtime_ticks_to_secs(9), 0);
        assert_eq!(mtime_ticks_to_secs(-5), 0);
        assert_eq!(mtime_ticks_to_secs(FILETIME_UNIX_EPOCH), 0);
        assert_eq!(mtime_ticks_to_secs(i64::MAX), u32::MAX);
        assert!(mtime_ticks_to_secs(ft) < mtime_ticks_to_secs(ft + 864_000_000_000));
    }

    #[test]
    fn next_day_handles_month_and_leap() {
        assert_eq!(
            Civil {
                y: 2024,
                m: 2,
                d: 28
            }
            .next_day(),
            Civil {
                y: 2024,
                m: 2,
                d: 29
            }
        );
        assert_eq!(
            Civil {
                y: 2023,
                m: 12,
                d: 31
            }
            .next_day(),
            Civil {
                y: 2024,
                m: 1,
                d: 1
            }
        );
    }

    #[test]
    fn utc_resolver_epoch() {
        assert_eq!(
            utc_midnight(Civil {
                y: 1970,
                m: 1,
                d: 1
            }),
            FILETIME_UNIX_EPOCH
        );
    }

    #[test]
    fn validity() {
        assert!(
            Civil {
                y: 2024,
                m: 2,
                d: 29
            }
            .is_valid()
        );
        assert!(
            !Civil {
                y: 2023,
                m: 2,
                d: 29
            }
            .is_valid()
        );
        assert!(
            !Civil {
                y: 2023,
                m: 13,
                d: 1
            }
            .is_valid()
        );
    }
}
