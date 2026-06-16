//! Civil-date ↔ FILETIME conversion for `dm:` filters.
//!
//! `dm:` bounds are interpreted in the *local* time zone
//! (docs/ARCHITECTURE.md C-4). The conversion is injected via
//! [`DateResolver`] so the parser/compiler stay pure and tests can use UTC.

/// FILETIME ticks (100 ns since 1601-01-01) at the Unix epoch (1970-01-01).
pub const FILETIME_UNIX_EPOCH: i64 = 116_444_736_000_000_000;
const TICKS_PER_SECOND: i64 = 10_000_000;

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
    pub const fn next_day(self) -> Self {
        civil_from_days(days_from_civil(self) + 1)
    }

    /// The first day of the month following this date's month.
    pub const fn first_of_next_month(self) -> Self {
        if self.m == 12 {
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

    /// True if the date is a real calendar date within FILETIME's range
    /// (year 1601..=9999, valid month, day within the month's length).
    pub fn is_valid(self) -> bool {
        if !(1601..=9999).contains(&self.y) || !(1..=12).contains(&self.m) {
            return false;
        }
        self.d >= 1 && self.d <= days_in_month(self.y, self.m)
    }
}

const fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
pub const fn days_from_civil(c: Civil) -> i64 {
    let y = if c.m <= 2 { c.y - 1 } else { c.y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (c.m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + c.d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`]: the civil date for a day count since
/// 1970-01-01 (Howard Hinnant's `civil_from_days`).
pub const fn civil_from_days(days: i64) -> Civil {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    Civil {
        y: (if m <= 2 { y + 1 } else { y }) as i32,
        m,
        d,
    }
}

/// Converts a civil date (midnight) to FILETIME ticks.
pub trait DateResolver {
    /// FILETIME ticks (100 ns since 1601-01-01) for midnight at the start of
    /// the given civil date, in this resolver's time zone.
    fn filetime_at_midnight(&self, c: Civil) -> i64;
}

/// Pure UTC resolver — deterministic, used by unit tests.
pub struct UtcResolver;

impl DateResolver for UtcResolver {
    fn filetime_at_midnight(&self, c: Civil) -> i64 {
        FILETIME_UNIX_EPOCH + days_from_civil(c) * 86_400 * TICKS_PER_SECOND
    }
}

/// Local-time-zone resolver backed by the Windows time-zone/DST rules.
#[cfg(windows)]
pub struct WindowsLocalResolver;

#[cfg(windows)]
impl DateResolver for WindowsLocalResolver {
    fn filetime_at_midnight(&self, c: Civil) -> i64 {
        use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows_sys::Win32::System::Time::{
            SystemTimeToFileTime, TzSpecificLocalTimeToSystemTime,
        };

        unsafe {
            let local = SYSTEMTIME {
                wYear: c.y as u16,
                wMonth: c.m as u16,
                wDayOfWeek: 0,
                wDay: c.d as u16,
                wHour: 0,
                wMinute: 0,
                wSecond: 0,
                wMilliseconds: 0,
            };
            let mut utc: SYSTEMTIME = std::mem::zeroed();
            let mut ft: FILETIME = std::mem::zeroed();
            if TzSpecificLocalTimeToSystemTime(std::ptr::null(), &raw const local, &raw mut utc)
                != 0
                && SystemTimeToFileTime(&raw const utc, &raw mut ft) != 0
            {
                ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
            } else {
                // Out-of-range dates: fall back to UTC math rather than failing
                // the whole query.
                UtcResolver.filetime_at_midnight(c)
            }
        }
    }
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
            UtcResolver.filetime_at_midnight(Civil {
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
