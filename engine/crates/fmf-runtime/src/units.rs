//! Units used by measurement and diagnostic adapters.

/// Bytes in one kibibyte.
pub const KIBIBYTE: f64 = 1024.0;
/// Bytes in one mebibyte.
pub const MEBIBYTE: f64 = KIBIBYTE * KIBIBYTE;
/// Display value when a measurement has no samples.
pub const EMPTY_MEASUREMENT: f64 = 0.0;
/// Seconds in one minute.
pub const SECONDS_PER_MINUTE: i64 = 60;
/// Seconds in one hour.
pub const SECONDS_PER_HOUR: i64 = 3600;
/// Seconds in one day.
pub const SECONDS_PER_DAY: i64 = 86_400;
/// Minutes in one hour.
pub const MINUTES_PER_HOUR: i32 = 60;
