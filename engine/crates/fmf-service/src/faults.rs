//! Fault injection for the pipe path, gated behind `--debug-faults` and off
//! by default (an installed service never enables it).
//!
//! Mirrors the fake engine's `!!` queries so the failure pipeline can be
//! exercised E2E:
//! `!!panic` (dispatch panic → `FMF_E_PANIC`, connection survives),
//! `!!drop` (abrupt disconnect → reconnect path), `!!lag` (page responses
//! +250ms → flicker-free publish path under RTT stress),
//! `!!warn` (one WARN into the diagnostics ring → `ENGINE_ERROR` event,
//! InfoBar and F12 health card; the query itself still runs).
//!
//! "Mirrors" is a contract, not a hope: the token set here and the fake's must
//! be identical, or a fault reproduces on one engine and not the other — and
//! the fake is the one developers reach for. `xtask`'s
//! `fault_injection_tokens_match_on_both_engines` holds both against one
//! reviewed list.

use std::time::Instant;

use crate::dispatch::Outcome;

/// Fault injection state for one service instance: whether `--debug-faults`
/// is enabled and the start time used to report uptime.
#[derive(Clone)]
pub struct Faults {
    enabled: bool,
    started: Instant,
}

impl Faults {
    /// Creates fault state, recording the current instant as the uptime
    /// origin. `enabled` reflects `--debug-faults`; it is off by default.
    #[must_use]
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            started: Instant::now(),
        }
    }

    /// Milliseconds elapsed since this instance was constructed.
    #[must_use]
    pub fn uptime_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Intercepts `!!panic` / `!!drop` query texts, and emits the `!!warn`
    /// diagnostic before letting that query run. `!!lag` is not intercepted —
    /// the query runs normally and the *pages* lag.
    ///
    /// # Panics
    /// Deliberately panics when faults are enabled and `text` is `!!panic` —
    /// the injected fault that exercises the `catch_unwind` → `FMF_E_PANIC`
    /// firewall.
    #[must_use]
    pub fn on_query(&self, text: &str) -> Option<Outcome> {
        if !self.enabled {
            return None;
        }
        match text {
            "!!panic" => panic!("fault injection: !!panic"),
            "!!drop" => Some(Outcome::Drop),
            "!!warn" => {
                // A plain warn, not `degrade!`: the macro's counter bump is a
                // real degradation statistic, and an injected fault must not
                // write into the numbers an operator reads. The WARN alone is
                // what reaches DiagLayer → ring → ENGINE_ERROR → the UI, which
                // is the path under test. The query then runs normally, so the
                // "warned but still served" case is exercised too.
                tracing::warn!(area = "fault", "fault injection: !!warn");
                None
            }
            _ => None,
        }
    }

    /// True when faults are enabled and `text` is `!!lag`, signalling that
    /// page responses for this query should be delayed by +250ms.
    #[must_use]
    pub fn lag_marker(&self, text: &str) -> bool {
        self.enabled && text == "!!lag"
    }
}
