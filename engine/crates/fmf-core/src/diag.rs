//! Process-wide diagnostics: capture every WARN+ tracing event and panic.
//!
//! Each one ends up in (a) the rolling log file, (b) a global ring buffer
//! surfaced through `MetricsSnapshot` → the F12 panel and `fmf stats`, and
//! (c) any registered sinks (the FFI forwards them as `ENGINE_ERROR` events
//! to the UI).
//! The layer responsible for the "don't go silent" arm of
//! "don't crash / don't hang / don't go silent".

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Once, OnceLock};
use std::time::Instant;

use parking_lot::Mutex;
use serde::Serialize;

/// Severity class of a captured diagnostic event, in ascending order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// A degraded path that recovered via fallback (the `degrade!` form).
    Warn,
    /// A handled failure surfaced to the operator.
    Error,
    /// A panic captured by the `install_panic_hook` backtrace path.
    Panic,
}

impl Severity {
    /// Numeric form carried in the POD FFI event payload.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        match self {
            Self::Warn => 1,
            Self::Error => 2,
            Self::Panic => 3,
        }
    }
}

/// One captured diagnostic event: the unit stored in the ring and fanned out
/// to sinks.
#[derive(Clone, Debug, Serialize)]
pub struct ErrorEvent {
    /// Monotonic sequence number, assigned per event from process start (1-based).
    pub seq: u64,
    /// Milliseconds since the first diagnostics call (process uptime, ms).
    pub uptime_ms: u64,
    /// Severity class of this event.
    pub severity: Severity,
    /// tracing target (module path) — "where".
    pub area: String,
    /// Volume the event pertains to (e.g. "C:"), when known.
    pub volume: Option<String>,
    /// Human-readable message — "what".
    pub message: String,
}

const RING_CAP: usize = 128;

static START: OnceLock<Instant> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(1);
static RING: Mutex<VecDeque<ErrorEvent>> = Mutex::new(VecDeque::new());
static SINKS: Mutex<Vec<(u64, Sink)>> = Mutex::new(Vec::new());
static SINK_IDS: AtomicU64 = AtomicU64::new(1);

type Sink = Arc<dyn Fn(&ErrorEvent) + Send + Sync>;

/// Register a fan-out target for new error events; dropping the guard
/// unregisters it (the FFI ties this to the engine handle's lifetime).
pub fn register_sink(sink: Sink) -> SinkGuard {
    let id = SINK_IDS.fetch_add(1, Ordering::Relaxed);
    SINKS.lock().push((id, sink));
    SinkGuard(id)
}

/// Lifetime handle for a registered sink; dropping it unregisters the sink.
pub struct SinkGuard(u64);

impl Drop for SinkGuard {
    fn drop(&mut self) {
        SINKS.lock().retain(|(id, _)| *id != self.0);
    }
}

/// Record one diagnostic event (ring + sinks). Normally reached via the
/// tracing layer rather than called directly.
pub fn record(severity: Severity, area: &str, volume: Option<String>, message: String) {
    let ev = ErrorEvent {
        seq: SEQ.fetch_add(1, Ordering::Relaxed),
        uptime_ms: START.get_or_init(Instant::now).elapsed().as_millis() as u64,
        severity,
        area: area.to_string(),
        volume,
        message,
    };
    {
        let mut ring = RING.lock();
        if ring.len() == RING_CAP {
            ring.pop_front();
        }
        ring.push_back(ev.clone());
    }
    // Snapshot the sinks first: a sink may log (or register) and must not
    // re-enter the lock.
    let sinks: Vec<Sink> = SINKS.lock().iter().map(|(_, s)| s.clone()).collect();
    for s in sinks {
        s(&ev);
    }
}

/// Snapshot of the diagnostics ring (oldest first), capped at the ring size.
pub fn recent_errors() -> Vec<ErrorEvent> {
    RING.lock().iter().cloned().collect()
}

// ── tracing layer ───────────────────────────────────────────────────────

/// Routes WARN/ERROR tracing events into the diagnostics ring + sinks.
/// Events with `target: "panic"` are classified as panics.
pub struct DiagLayer;

struct FieldGrab {
    message: String,
    volume: Option<String>,
    area: Option<String>,
}

impl tracing::field::Visit for FieldGrab {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "volume" => self.volume = Some(format!("{value:?}").trim_matches('"').to_string()),
            "area" => self.area = Some(format!("{value:?}").trim_matches('"').to_string()),
            _ => {}
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_string(),
            "volume" => self.volume = Some(value.to_string()),
            "area" => self.area = Some(value.to_string()),
            _ => {}
        }
    }
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for DiagLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = *event.metadata().level();
        if level > tracing::Level::WARN {
            return; // Level orders ERROR < WARN < INFO …
        }
        let mut grab = FieldGrab {
            message: String::new(),
            volume: None,
            area: None,
        };
        event.record(&mut grab);
        let target = event.metadata().target();
        let severity = if target == "panic" {
            Severity::Panic
        } else if level == tracing::Level::ERROR {
            Severity::Error
        } else {
            Severity::Warn
        };
        // The logical `area` field wins over the module path so the ring shows
        // the same "where" tag as the log file; panics keep their target.
        let area = grab.area.as_deref().unwrap_or(target);
        record(severity, area, grab.volume, grab.message);
    }
}

// ── logfmt formatting ───────────────────────────────────────────────────

/// Cap on one field value's source length (bytes), mirroring [`error_chain`]'s
/// 4 KiB cap: a pathological filename or query must not balloon a log line.
/// Truncation is marked with `…`.
const VALUE_CAP: usize = 1024;

/// Append `value` to `out` as one logfmt value: emitted bare when safe, else
/// wrapped in `"…"` with control characters escaped.
///
/// This is the single log-injection defence. Any CR/LF or other control byte
/// is escaped (`\r`/`\n`/`\u00XX`), so a value carrying newlines — a crafted
/// query, or an NTFS name with embedded control chars — can never forge a
/// second log line.
fn escape_value(out: &mut String, value: &str) {
    let (value, truncated) = if value.len() > VALUE_CAP {
        let mut end = VALUE_CAP;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        (&value[..end], true)
    } else {
        (value, false)
    };
    let needs_quote = truncated
        || value.is_empty()
        || value
            .bytes()
            .any(|b| b == b' ' || b == b'=' || b == b'"' || b == b'\\' || b < 0x20);
    if !needs_quote {
        out.push_str(value);
        return;
    }
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    if truncated {
        out.push('…');
    }
    out.push('"');
}

/// Append one ` key=value` pair (leading space) with the value escaped.
fn push_field(out: &mut String, key: &str, value: &str) {
    out.push(' ');
    out.push_str(key);
    out.push('=');
    escape_value(out, value);
}

/// Local UTC offset in minutes, captured once at process start.
///
/// Resolving the zone per line would dominate the formatter; a DST boundary
/// crossed mid-process is an accepted trade-off for log timestamps.
fn tz_offset_minutes() -> i32 {
    static OFFSET: OnceLock<i32> = OnceLock::new();
    *OFFSET.get_or_init(compute_tz_offset_minutes)
}

#[cfg(windows)]
fn compute_tz_offset_minutes() -> i32 {
    use windows_sys::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    // GetTimeZoneInformation's return value; 2 == TIME_ZONE_ID_DAYLIGHT.
    const TIME_ZONE_ID_DAYLIGHT: u32 = 2;
    // SAFETY: a zeroed TIME_ZONE_INFORMATION is a valid initial value and the
    // call only writes into it.
    unsafe {
        let mut tzi: TIME_ZONE_INFORMATION = std::mem::zeroed();
        let id = GetTimeZoneInformation(&raw mut tzi);
        // `Bias` is the minutes to ADD to local time to reach UTC, so the local
        // offset is its negation plus the active seasonal bias.
        let seasonal = if id == TIME_ZONE_ID_DAYLIGHT {
            tzi.DaylightBias
        } else {
            tzi.StandardBias
        };
        -(tzi.Bias + seasonal)
    }
}

#[cfg(not(windows))]
fn compute_tz_offset_minutes() -> i32 {
    0 // No Win32 zone API off-Windows (fuzz/CI hosts); stamp as UTC.
}

/// Append an RFC3339 timestamp with the local zone offset
/// (`2026-06-30T12:34:56.789+09:00`) — the logfmt line's first column. Reuses
/// the index's own civil-date math so no date crate is pulled in.
fn write_ts(out: &mut String) {
    use std::fmt::Write as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let offset_min = tz_offset_minutes();
    let local_secs = now.as_secs() as i64 + i64::from(offset_min) * 60;
    let civil = crate::query::dates::civil_from_days(local_secs.div_euclid(86_400));
    let tod = local_secs.rem_euclid(86_400);
    let (sign, off_abs) = if offset_min >= 0 {
        ('+', offset_min)
    } else {
        ('-', -offset_min)
    };
    let _ = write!(
        out,
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}{}{:02}:{:02}",
        civil.y,
        civil.m,
        civil.d,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        now.subsec_millis(),
        sign,
        off_abs / 60,
        off_abs % 60,
    );
}

/// Fixed-width (5-char) level tag so columns line up across the levels.
const fn level_tag(level: tracing::Level) -> &'static str {
    match level {
        tracing::Level::ERROR => "ERROR",
        tracing::Level::WARN => "WARN ",
        tracing::Level::INFO => "INFO ",
        tracing::Level::DEBUG => "DEBUG",
        tracing::Level::TRACE => "TRACE",
    }
}

/// Visits tracing fields, rendering them as logfmt. For event lines
/// (`reserve = true`) `message` and `area` are pulled out for fixed-position
/// emission; for span fields (`reserve = false`) everything is an inline pair.
struct LogfmtVisitor {
    reserve: bool,
    message: Option<String>,
    area: Option<String>,
    fields: String,
}

impl LogfmtVisitor {
    const fn event() -> Self {
        Self {
            reserve: true,
            message: None,
            area: None,
            fields: String::new(),
        }
    }

    const fn span() -> Self {
        Self {
            reserve: false,
            message: None,
            area: None,
            fields: String::new(),
        }
    }

    fn put(&mut self, name: &str, value: &str) {
        match (self.reserve, name) {
            (true, "message") => self.message = Some(value.to_string()),
            (true, "area") => self.area = Some(value.to_string()),
            _ => push_field(&mut self.fields, name, value),
        }
    }

    fn put_display<T: std::fmt::Display>(&mut self, name: &str, value: T) {
        use std::fmt::Write as _;
        // Numbers and bools never need quoting; write them straight.
        let _ = write!(self.fields, " {name}={value}");
    }
}

impl tracing::field::Visit for LogfmtVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.put(field.name(), value);
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.put(field.name(), &format!("{value:?}"));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.put_display(field.name(), value);
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.put_display(field.name(), value);
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.put_display(field.name(), value);
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.put_display(field.name(), value);
    }
}

/// Field formatter that renders span fields (e.g. the per-query `qid`) in
/// logfmt, so they interleave consistently with the event line built by
/// [`LogfmtFormat`]. Each pair is stored with a leading space.
struct LogfmtFields;

impl<'writer> tracing_subscriber::fmt::FormatFields<'writer> for LogfmtFields {
    fn format_fields<R: tracing_subscriber::field::RecordFields>(
        &self,
        mut writer: tracing_subscriber::fmt::format::Writer<'writer>,
        fields: R,
    ) -> std::fmt::Result {
        let mut visitor = LogfmtVisitor::span();
        fields.record(&mut visitor);
        writer.write_str(&visitor.fields)
    }
}

/// Event formatter producing one logfmt line:
/// `ts level area [span fields] [event fields] msg=…`.
struct LogfmtFormat;

impl<S, N> tracing_subscriber::fmt::FormatEvent<S, N> for LogfmtFormat
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    N: for<'a> tracing_subscriber::fmt::FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &tracing_subscriber::fmt::FmtContext<'_, S, N>,
        mut writer: tracing_subscriber::fmt::format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let meta = event.metadata();
        let mut visitor = LogfmtVisitor::event();
        event.record(&mut visitor);

        let mut line = String::new();
        write_ts(&mut line);
        line.push(' ');
        line.push_str(level_tag(*meta.level()));

        // area: an explicit field wins, else the target's last path segment.
        line.push_str(" area=");
        let area = visitor.area.clone().unwrap_or_else(|| {
            meta.target()
                .rsplit("::")
                .next()
                .unwrap_or_else(|| meta.target())
                .to_string()
        });
        escape_value(&mut line, &area);

        // span fields (outer → inner) — carries `qid` from the per-query span.
        if let Some(scope) = ctx.event_scope() {
            for span in scope.from_root() {
                if let Some(fields) = span
                    .extensions()
                    .get::<tracing_subscriber::fmt::FormattedFields<N>>()
                {
                    line.push_str(&fields.fields);
                }
            }
        }

        // event fields (each already carries its leading space)
        line.push_str(&visitor.fields);

        // message last
        if let Some(msg) = &visitor.message {
            line.push_str(" msg=");
            escape_value(&mut line, msg);
        }

        writeln!(writer, "{line}")
    }
}

// ── panic hook & logging bootstrap ──────────────────────────────────────

/// Route every panic (any thread) through tracing with a backtrace, so it
/// reaches the log file, the ring and the UI. Idempotent.
pub fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let payload = info
                .payload()
                .downcast_ref::<&str>()
                .map(ToString::to_string)
                .or_else(|| info.payload().downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "<non-string panic payload>".to_string());
            let location = info.location().map_or_else(
                || "<unknown>".to_string(),
                |l| format!("{}:{}", l.file(), l.line()),
            );
            let backtrace = std::backtrace::Backtrace::force_capture();
            tracing::error!(
                target: "panic",
                "panic at {location}: {payload}\n{backtrace}"
            );
            previous(info);
        }));
    });
}

/// Resolves the engine log directory.
///
/// An explicit override (config key, CLI flag) wins; otherwise the log sits in
/// a `logs` subdir of `index_dir` — the location the caller already chose to
/// write the index, so it shares the index's writability and pollution domain
/// (portable → `<exe>\data\index\logs`, scope → `%LOCALAPPDATA%\…\index\logs`).
/// There is deliberately **no machine-wide default**: falling back to
/// `%ProgramData%` dirtied the machine for non-elevated callers and panicked
/// when that dir existed but was not writable. The machine service does not rely
/// on this fallback — it passes its own `%ProgramData%\find-my-files\logs`
/// explicitly. The one implementation of this rule — every entry point resolves
/// through here (ADR-0018; the rule's prose lives in docs/ARCHITECTURE.md).
#[must_use]
pub fn resolve_log_dir(
    explicit: Option<std::path::PathBuf>,
    index_dir: &std::path::Path,
) -> std::path::PathBuf {
    explicit.unwrap_or_else(|| index_dir.join("logs"))
}

/// Retained daily `engine.<date>.log` generations for the long-lived machine
/// service (≈ two weeks at one file/day).
pub const SERVICE_MAX_LOG_FILES: usize = 14;

/// Retained daily `engine.<date>.log` generations for FFI/CLI callers (≈ one
/// week; these run far less than the resident service).
pub const DEFAULT_MAX_LOG_FILES: usize = 7;

/// The one diagnostics bootstrap: file/stderr logging + panic capture +
/// diag-ring wiring, idempotent — FFI `fmf_engine_create`, the service
/// entry points and the CLI all call exactly this (ADR-0018).
///
/// `max_log_files` caps the retained daily `engine.<date>.log` generations
/// (ignored when `log_dir` is `None`, i.e. the CLI's stderr path).
pub fn init_diag(log_dir: Option<&std::path::Path>, level: &str, max_log_files: usize) {
    init_logging(log_dir, level, max_log_files);
    install_panic_hook();
}

/// Full error cause chain as one line.
///
/// The single implementation behind the FFI `fmf_last_error` detail and the
/// pipe error-response payload. Capped at 4 KiB (a pathological chain of
/// nested I/O errors must not balloon a frame); the cap is part of the
/// contract's error-detail spec.
pub fn error_chain(e: &dyn std::error::Error) -> String {
    const CAP: usize = 4096;
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(cause) = src {
        s.push_str(" — caused by: ");
        s.push_str(&cause.to_string());
        src = cause.source();
    }
    if s.len() > CAP {
        let mut end = CAP;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push('…');
    }
    s
}

/// The one way to record a degraded path (one that recovers via fallback):
/// warn and counter increment done atomically (the syntactic form of
/// "don't go silent" — ADR-0018).
///
/// `rg degrade!` enumerates every degraded path. Batch paths (scan internals)
/// return degradation via stats fields and the worker layer maps them to
/// counters in one place (don't scatter the macro across the hot path).
#[macro_export]
macro_rules! degrade {
    ($counter:expr, $($arg:tt)*) => {{
        $crate::metrics::Counters::bump(&$counter);
        tracing::warn!($($arg)*);
    }};
}

/// Emit the one structured per-query observability line, called by each
/// transport once it has allocated the result handle `rid`.
///
/// The ambient `qid` span the caller entered (pipe request id / FFI counter)
/// ties this to the UI's own `app.log` line; pipe correlates by `qid`, the
/// in-process FFI path by `rid`. The query *text* is deliberately omitted —
/// only its length — because filenames and queries are the sensitive asset
/// (redaction; ADR-0037). Skipped for an unchanged idle USN-driven requery so
/// the log does not churn while the UI sits on a `RefreshInPlace`.
pub fn log_query_served(rid: u64, trace: &crate::metrics::QueryTrace) {
    if trace.unchanged {
        return;
    }
    tracing::info!(
        area = "query",
        rid = rid,
        hits = trace.hits,
        qlen = trace.query.chars().count() as u64,
        dur_us = trace.total_us,
        volumes = trace.volumes,
        driver = %trace.driver,
        cache = %trace.cache,
        "query served"
    );
}

static LOG_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

/// Initialize process-wide logging once.
///
/// `log_dir = Some(..)` writes a daily rolling `engine.<date>.log` capped at
/// `max_log_files` generations; `None` writes to stderr (CLI). Every line is
/// logfmt (see [`LogfmtFormat`]). The `FMF_LOG` env var overrides `level`.
/// Safe to call repeatedly — later calls no-op.
pub fn init_logging(log_dir: Option<&std::path::Path>, level: &str, max_log_files: usize) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    START.get_or_init(Instant::now);
    let filter = tracing_subscriber::EnvFilter::try_from_env("FMF_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    let registry = tracing_subscriber::registry().with(filter).with(DiagLayer);

    // A degraded last resort shared by every dir/appender failure: stderr +
    // a breadcrumb in the ring, rather than failing the whole process.
    let stderr_layer = || {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .fmt_fields(LogfmtFields)
            .event_format(LogfmtFormat)
            .with_writer(std::io::stderr)
    };

    match log_dir {
        Some(dir) => {
            if let Err(e) = std::fs::create_dir_all(dir) {
                record(
                    Severity::Error,
                    "diag",
                    None,
                    format!("cannot create log dir {}: {e}", dir.display()),
                );
                let _ = registry.with(stderr_layer()).try_init();
                return;
            }
            let appender = match tracing_appender::rolling::RollingFileAppender::builder()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix("engine")
                .filename_suffix("log")
                .max_log_files(max_log_files)
                .build(dir)
            {
                Ok(appender) => appender,
                Err(e) => {
                    record(
                        Severity::Error,
                        "diag",
                        None,
                        format!("cannot open log file in {}: {e}", dir.display()),
                    );
                    let _ = registry.with(stderr_layer()).try_init();
                    return;
                }
            };
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let _ = registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .fmt_fields(LogfmtFields)
                        .event_format(LogfmtFormat)
                        .with_writer(writer),
                )
                .try_init();
            let _ = LOG_GUARD.set(guard);
        }
        None => {
            let _ = registry.with(stderr_layer()).try_init();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Single test for the global pipeline (ring/sinks/hook are process-wide
    /// state; parallel tests would interleave).
    #[test]
    fn ring_sinks_layer_and_panic_hook() {
        use tracing_subscriber::layer::SubscriberExt;

        // Layer → ring + sink.
        let seen = Arc::new(Mutex::new(Vec::<ErrorEvent>::new()));
        let seen2 = seen.clone();
        let guard = register_sink(Arc::new(move |ev| seen2.lock().push(ev.clone())));

        let subscriber = tracing_subscriber::registry().with(DiagLayer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::warn!(volume = "C:", "snapshot stale");
            tracing::warn!(area = "pipe", "reconnect");
            tracing::error!("boom");
            tracing::info!("ignored");

            // The hook fires during unwinding on this thread (with_default
            // is thread-local, so a spawned thread would miss the layer).
            install_panic_hook();
            let _ = std::panic::catch_unwind(|| panic!("test panic"));
        });

        let events = seen.lock().clone();
        assert!(events.len() >= 4, "expected 4+ events, got {events:?}");
        let warn = events
            .iter()
            .find(|e| e.message.contains("snapshot stale"))
            .unwrap();
        assert_eq!(warn.volume.as_deref(), Some("C:"));
        // No explicit `area` → the event falls back to the module target.
        assert!(
            warn.area.contains("diag"),
            "fallback area, got {}",
            warn.area
        );
        // An explicit `area` field wins over the module target.
        let pipe = events
            .iter()
            .find(|e| e.message.contains("reconnect"))
            .unwrap();
        assert_eq!(pipe.area, "pipe");
        assert!(events.iter().any(|e| e.severity == Severity::Error));
        let panic_ev = events
            .iter()
            .find(|e| e.severity == Severity::Panic)
            .expect("panic captured");
        assert!(panic_ev.message.contains("test panic"));
        assert!(panic_ev.message.contains("diag.rs") || panic_ev.message.contains("backtrace"));

        // Ring kept them too, with monotonically increasing seq.
        let ring = recent_errors();
        assert!(ring.len() >= 3);
        assert!(ring.windows(2).all(|w| w[0].seq < w[1].seq));

        // Dropping the guard unregisters the sink.
        drop(guard);
        let before = seen.lock().len();
        record(Severity::Warn, "t", None, "after-drop".into());
        assert_eq!(seen.lock().len(), before);
    }

    #[test]
    fn log_dir_defaults_next_to_the_index_never_machine_wide() {
        use std::path::{Path, PathBuf};

        let index = Path::new("some").join("writable").join("index");

        // Default: the log co-locates under the caller's index dir, so it
        // inherits the index's writability and pollution domain.
        assert_eq!(resolve_log_dir(None, &index), index.join("logs"));

        // An explicit override (config/CLI) always wins.
        let explicit = PathBuf::from("elsewhere").join("logs");
        assert_eq!(resolve_log_dir(Some(explicit.clone()), &index), explicit);

        // Never falls back to a hard-coded machine-wide ProgramData path
        // (that dirtied the machine for non-elevated callers and panicked when
        // unwritable).
        assert!(
            !resolve_log_dir(None, &index)
                .to_string_lossy()
                .contains("ProgramData")
        );
    }

    #[test]
    fn escape_value_leaves_safe_values_bare() {
        for safe in ["C:", "query-served_7f3a", "1240", "C:,D:"] {
            let mut s = String::new();
            escape_value(&mut s, safe);
            assert_eq!(s, safe);
        }
    }

    #[test]
    fn escape_value_quotes_and_escapes() {
        let cases = [
            ("a b", "\"a b\""),
            ("k=v", "\"k=v\""),
            ("say \"hi\"", "\"say \\\"hi\\\"\""),
            ("back\\slash", "\"back\\\\slash\""),
            ("", "\"\""),
            ("tab\there", "\"tab\\there\""),
        ];
        for (input, want) in cases {
            let mut s = String::new();
            escape_value(&mut s, input);
            assert_eq!(s, want, "input {input:?}");
        }
    }

    #[test]
    fn escape_value_neutralises_log_injection() {
        // CR/LF must fold onto one line so a crafted value cannot forge a
        // second log record.
        let mut s = String::new();
        escape_value(&mut s, "real\r\nFAKE level=ERROR msg=pwned");
        assert!(!s.contains('\n') && !s.contains('\r'), "got {s}");
        assert!(s.contains("\\r\\n"), "got {s}");

        // A bare control char becomes \u00XX.
        let mut s = String::new();
        escape_value(&mut s, "x\u{0007}y");
        assert_eq!(s, "\"x\\u0007y\"");
    }

    #[test]
    fn escape_value_caps_long_values_on_a_char_boundary() {
        // Multibyte input so the cap lands mid-character and must back up.
        let long = "あ".repeat(VALUE_CAP);
        let mut s = String::new();
        escape_value(&mut s, &long);
        assert!(s.ends_with("…\""), "should be marked truncated");
        assert!(s.len() < long.len(), "should be shorter than the source");
        // Round-trips as valid UTF-8 (no panic above already proves it).
        assert!(s.starts_with('"'));
    }

    #[test]
    fn write_ts_is_rfc3339_with_offset() {
        let mut s = String::new();
        write_ts(&mut s);
        // e.g. 2026-06-30T12:34:56.789+09:00
        assert_eq!(s.len(), 29, "got {s:?}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        assert_eq!(&s[19..20], ".");
        assert!(matches!(&s[23..24], "+" | "-"), "offset sign in {s}");
        assert_eq!(&s[26..27], ":");
    }

    #[test]
    fn logfmt_line_orders_fields_and_carries_span_qid() {
        use std::io::Write;
        use tracing_subscriber::fmt::MakeWriter;
        use tracing_subscriber::layer::SubscriberExt;

        #[derive(Clone)]
        struct VecWriter(Arc<Mutex<Vec<u8>>>);
        impl Write for VecWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> MakeWriter<'a> for VecWriter {
            type Writer = Self;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }

        let buf = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .fmt_fields(LogfmtFields)
                .event_format(LogfmtFormat)
                .with_writer(VecWriter(buf.clone())),
        );
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("q", qid = "7f3a");
            let _g = span.enter();
            tracing::info!(
                area = "query",
                vol = "C:",
                hits = 1240_u64,
                dur_ms = 8_u64,
                "query served"
            );
        });

        let out = String::from_utf8(buf.lock().clone()).unwrap();
        let line = out.lines().next().expect("one line");
        assert!(line.contains(" INFO  area=query"), "level+area: {line}");
        assert!(line.contains(" qid=7f3a"), "span qid: {line}");
        assert!(
            line.contains(" vol=C: hits=1240 dur_ms=8 "),
            "event fields: {line}"
        );
        assert!(line.ends_with(" msg=\"query served\""), "msg last: {line}");
        // The span field (qid) precedes the event fields.
        assert!(
            line.find("qid=").unwrap() < line.find("hits=").unwrap(),
            "span field should come before event fields: {line}"
        );
    }
}
