//! Engine bring-up for the service: the lock-loser path retries with
//! backoff instead of dying (an in-proc UI may legitimately hold the index;
//! docs/ARCHITECTURE.md「Pipe プロトコル」§単一書き手の排他).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use fmf_core::engine::{Engine, EngineConfig, EngineCreateError};

const FIRST_DELAY: Duration = Duration::from_secs(5);
const MAX_DELAY: Duration = Duration::from_mins(1);

/// Retries on `Locked` until success, a non-lock error, `stop`, or
/// `max_attempts`. Exhaustion returns the `Locked` error — the caller exits
/// honestly (no SCM crash-loop; P4 wires the exit code).
///
/// # Errors
/// Returns the [`EngineCreateError`]: a non-`Locked` error immediately, or the
/// last `Locked` error once `max_attempts` is reached or `stop` is set.
pub fn create_engine_with_retry(
    index_dir: PathBuf,
    stop: &AtomicBool,
    max_attempts: u32,
) -> Result<Arc<Engine>, EngineCreateError> {
    let mut delay = FIRST_DELAY;
    let mut attempt = 0u32;
    loop {
        match Engine::new(EngineConfig {
            index_dir: index_dir.clone(),
        }) {
            Ok(e) => return Ok(e),
            Err(e @ EngineCreateError::Locked(_)) => {
                attempt += 1;
                tracing::warn!(
                    error = %e,
                    attempt,
                    max_attempts,
                    retry_in_s = delay.as_secs(),
                    "index dir locked by another engine"
                );
                if attempt >= max_attempts || stop.load(Ordering::Relaxed) {
                    return Err(e);
                }
                std::thread::sleep(delay);
                delay = (delay * 2).min(MAX_DELAY);
            }
            Err(e) => return Err(e),
        }
    }
}
