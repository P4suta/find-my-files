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
}

impl tracing::field::Visit for FieldGrab {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        match field.name() {
            "message" => self.message = format!("{value:?}"),
            "volume" => self.volume = Some(format!("{value:?}").trim_matches('"').to_string()),
            _ => {}
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_string(),
            "volume" => self.volume = Some(value.to_string()),
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
        record(severity, target, grab.volume, grab.message);
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

/// The one diagnostics bootstrap: file/stderr logging + panic capture +
/// diag-ring wiring, idempotent — FFI `fmf_engine_create`, the service
/// entry points and the CLI all call exactly this (ADR-0018).
pub fn init_diag(log_dir: Option<&std::path::Path>, level: &str) {
    init_logging(log_dir, level);
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

static LOG_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

/// Initialize process-wide logging once.
///
/// `log_dir = Some(..)` writes a daily rolling `engine.log`; `None` writes to
/// stderr (CLI). The `FMF_LOG` env var overrides `level`. Safe to call
/// repeatedly — later calls no-op.
pub fn init_logging(log_dir: Option<&std::path::Path>, level: &str) {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    START.get_or_init(Instant::now);
    let filter = tracing_subscriber::EnvFilter::try_from_env("FMF_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(level));
    let registry = tracing_subscriber::registry().with(filter).with(DiagLayer);

    match log_dir {
        Some(dir) => {
            if let Err(e) = std::fs::create_dir_all(dir) {
                // Last resort: stderr, and leave a breadcrumb in the ring.
                record(
                    Severity::Error,
                    "diag",
                    None,
                    format!("cannot create log dir {}: {e}", dir.display()),
                );
                let _ = registry
                    .with(
                        tracing_subscriber::fmt::layer()
                            .with_ansi(false)
                            .with_writer(std::io::stderr),
                    )
                    .try_init();
                return;
            }
            let appender = tracing_appender::rolling::daily(dir, "engine.log");
            let (writer, guard) = tracing_appender::non_blocking(appender);
            let _ = registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_writer(writer),
                )
                .try_init();
            let _ = LOG_GUARD.set(guard);
        }
        None => {
            let _ = registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_ansi(false)
                        .with_writer(std::io::stderr),
                )
                .try_init();
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
            tracing::error!("boom");
            tracing::info!("ignored");

            // The hook fires during unwinding on this thread (with_default
            // is thread-local, so a spawned thread would miss the layer).
            install_panic_hook();
            let _ = std::panic::catch_unwind(|| panic!("test panic"));
        });

        let events = seen.lock().clone();
        assert!(events.len() >= 3, "expected 3+ events, got {events:?}");
        let warn = events
            .iter()
            .find(|e| e.severity == Severity::Warn)
            .unwrap();
        assert_eq!(warn.volume.as_deref(), Some("C:"));
        assert!(warn.message.contains("snapshot stale"));
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
}
