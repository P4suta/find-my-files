//! Fault injection for the pipe path, gated behind `--debug-faults` and off
//! by default (an installed service never enables it). Mirrors the fake
//! engine's `!!` queries so the failure pipeline can be exercised E2E:
//! `!!panic` (dispatch panic → FMF_E_PANIC, connection survives),
//! `!!drop` (abrupt disconnect → reconnect path), `!!lag` (page responses
//! +250ms → flicker-free publish path under RTT stress).

use std::time::Instant;

use crate::dispatch::Outcome;

#[derive(Clone)]
pub struct Faults {
    enabled: bool,
    started: Instant,
}

impl Faults {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            started: Instant::now(),
        }
    }

    pub fn uptime_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Intercepts `!!panic` / `!!drop` query texts. `!!lag` is not
    /// intercepted — the query runs normally and the *pages* lag.
    pub fn on_query(&self, text: &str) -> Option<Outcome> {
        if !self.enabled {
            return None;
        }
        match text {
            "!!panic" => panic!("fault injection: !!panic"),
            "!!drop" => Some(Outcome::Drop),
            _ => None,
        }
    }

    pub fn lag_marker(&self, text: &str) -> bool {
        self.enabled && text == "!!lag"
    }
}
