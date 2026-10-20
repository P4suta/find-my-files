//! Time-zone adaptation for civil dates.
pub use fmf_core::dates::*;
/// Converts a civil date (midnight) to FILETIME ticks.
pub trait DateResolver {
    /// FILETIME ticks (100 ns since 1601-01-01) for midnight at the start of the given civil date, in this resolver's time zone.
    fn filetime_at_midnight(&self, c: Civil) -> i64;
}

/// Pure UTC resolver — deterministic, used by unit tests.
pub struct UtcResolver;

impl DateResolver for UtcResolver {
    fn filetime_at_midnight(&self, c: Civil) -> i64 {
        utc_midnight(c)
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
                ((ft.dwHighDateTime as i64) << u32::BITS) | ft.dwLowDateTime as i64
            } else {
                UtcResolver.filetime_at_midnight(c)
            }
        }
    }
}
