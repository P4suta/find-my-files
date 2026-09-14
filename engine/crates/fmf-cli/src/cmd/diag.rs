//! `diag` — versions, log locations, and the diagnostics a developer is
//! actually looking for.
//!
//! The command used to print one thing: `fmf_core::diag::recent_errors()`. That
//! ring is per-process and this process is `fmf` itself, freshly started and
//! holding no index — and `DiagLayer` only records WARN+, so the ring is empty
//! on essentially every run. The label said "in-process" and was therefore
//! honest, but the diagnostics worth reading live in the **service** process,
//! which `fmf` cannot reach.
//!
//! So `diag` reads what the service leaves behind: the rolling
//! `engine.<date>.log` under its data root. That needs no wire — no pipe client
//! in the CLI, no contract field — and it outlives the process that wrote it,
//! which the ring does not: a service that crashed and restarted has an empty
//! ring and a log that still says why. The ring is kept alongside it, now
//! labelled with whose ring it is, because the two are genuinely different
//! sources and a reader must not have to guess which one they are looking at.

use std::path::{Path, PathBuf};

use super::ctx::Ctx;
use super::json;

/// Trailing WARN/ERROR lines reported from the service log.
const SERVICE_LOG_LINES: usize = 20;

/// Bytes read from the end of each inspected generation. A busy service's daily
/// log is unbounded; only its end can matter here.
const TAIL_BYTES: u64 = 256 * 1024;

/// Newest generations inspected. One is not enough: a service that rolled over
/// at midnight has an almost-empty newest file while the failure being chased
/// sits in yesterday's.
const GENERATIONS: usize = 3;

/// The machine-readable shape of `diag --format json`.
#[derive(serde::Serialize)]
struct DiagReport {
    version: &'static str,
    arch: &'static str,
    engine_log_dir: String,
    app_log: String,
    log_filter: &'static str,
    /// This `fmf` process's own diagnostics ring — normally empty, and never
    /// the service's. Kept for compatibility and for `fmf -v` runs that do
    /// warn.
    recent_errors: serde_json::Value,
    /// What the service process left in its log — the diagnostics that survive
    /// a restart.
    service_log: ServiceLog,
}

/// The service's rolling log, as `diag` reports it.
#[derive(Default, serde::Serialize)]
struct ServiceLog {
    /// Directory searched for `engine.<date>.log` generations.
    dir: String,
    /// Generation file names inspected, oldest first.
    files: Vec<String>,
    /// Why the answer may be incomplete — unreadable directory, no log at all,
    /// a generation that could not be opened. Empty when `recent` is the whole
    /// truth. An empty section with no explanation is the failure this command
    /// exists to stop, so every reason is reported rather than swallowed.
    notes: Vec<String>,
    /// Trailing WARN/ERROR lines, oldest first.
    recent: Vec<String>,
}

/// The 5-character level column of a logfmt line, or `None` when the line has
/// no such column (a wrapped payload, a torn final write).
///
/// `LogfmtFormat` writes a 29-character RFC3339 stamp, one space, then a
/// fixed-width level tag — so the level is read by *position*. Substring
/// matching would promote any line whose message body happened to contain the
/// word `ERROR`, including one an attacker-supplied filename put there.
fn level_of(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    if bytes.len() < 35 || bytes[29] != b' ' {
        return None;
    }
    line.get(30..35)
}

/// Whether this log line is one of the ones worth reporting.
fn is_problem(line: &str) -> bool {
    matches!(level_of(line), Some("WARN " | "ERROR"))
}

/// Whether `name` is one of the appender's `engine.<date>.log` generations.
///
/// Matched case-insensitively: NTFS is, so the same file can be listed under
/// any casing, and a case-sensitive test would silently report "no log here"
/// for a log that exists.
fn is_generation(name: &str) -> bool {
    const PREFIX: &str = "engine.";
    name.get(..PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PREFIX))
        && Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("log"))
}

/// The newest [`GENERATIONS`] `engine.<date>.log` files, oldest first.
fn generations(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_generation)
        })
        .collect();
    // `engine.YYYY-MM-DD.log` sorts lexicographically in date order.
    files.sort();
    let skip = files.len().saturating_sub(GENERATIONS);
    Ok(files.split_off(skip))
}

/// The last [`TAIL_BYTES`] of `path`, as whole lines.
fn tail_lines(path: &Path) -> std::io::Result<Vec<String>> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let start = file.metadata()?.len().saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    // Bounded at the reader, not only by the seek: the service is still writing
    // to this file, so the length behind `start` is a guess that can grow while
    // it is being read. The slack lets concurrent growth through while keeping
    // the read finite.
    (&mut file)
        .take(TAIL_BYTES.saturating_mul(2))
        .read_to_end(&mut buf)?;
    // Lossy: a torn multi-byte write at the seek point must not lose the whole
    // tail, and the line it damages is dropped below anyway.
    let text = String::from_utf8_lossy(&buf).into_owned();
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // the seek landed mid-line
    }
    Ok(lines)
}

/// Collect the service's recent WARN/ERROR lines, reporting every reason the
/// answer might be short.
fn read_service_log(dir: &Path) -> ServiceLog {
    let mut log = ServiceLog {
        dir: dir.display().to_string(),
        ..ServiceLog::default()
    };
    let files = match generations(dir) {
        Ok(files) => files,
        Err(e) => {
            log.notes.push(format!(
                "cannot read {}: {e} — an installed service keeps this folder \
                 SYSTEM+Administrators only, so an unelevated shell sees this",
                dir.display()
            ));
            return log;
        }
    };
    if files.is_empty() {
        log.notes.push(format!(
            "no engine.<date>.log under {} — the service has not run here",
            dir.display()
        ));
        return log;
    }

    let mut problems = Vec::new();
    for path in &files {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into(),
        );
        match tail_lines(path) {
            Ok(lines) => {
                log.files.push(name);
                problems.extend(lines.into_iter().filter(|line| is_problem(line)));
            }
            Err(e) => log.notes.push(format!("cannot read {name}: {e}")),
        }
    }
    let skip = problems.len().saturating_sub(SERVICE_LOG_LINES);
    log.recent = problems.split_off(skip);
    log
}

pub fn diag(ctx: Ctx) -> Result<(), Box<dyn std::error::Error>> {
    let program_data = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
    let engine_log_dir = format!(r"{program_data}\find-my-files\logs");
    let app_log = r"%APPDATA%\find-my-files\logs\app.log".to_owned();
    let errors = fmf_core::diag::recent_errors();
    let service_log = read_service_log(Path::new(&engine_log_dir));

    if ctx.is_json() {
        return json::emit(&DiagReport {
            // Channel-aware build identity, identical to `fmf --version` (clap reads
            // the same const) — not the bare CARGO_PKG_VERSION, which dropped the
            // channel/sha and disagreed with --version.
            version: fmf_buildstamp::VERSION,
            arch: std::env::consts::ARCH,
            engine_log_dir,
            app_log,
            log_filter: "FMF_LOG",
            recent_errors: serde_json::to_value(&errors)?,
            service_log,
        });
    }

    println!(
        "fmf {} ({})",
        fmf_buildstamp::VERSION,
        std::env::consts::ARCH
    );
    println!("engine logs: {engine_log_dir} (rolling engine.<date>.log)");
    println!("app log    : {app_log}");
    println!("log filter : FMF_LOG (env var, e.g. FMF_LOG=debug)");

    // Which process each section belongs to is stated, because the two are not
    // interchangeable and the useful one is almost never the ring.
    println!(
        "\nservice process — WARN/ERROR from {} ({}):",
        if service_log.files.is_empty() {
            "no log file".to_owned()
        } else {
            service_log.files.join(", ")
        },
        service_log.recent.len()
    );
    for note in &service_log.notes {
        println!("  ! {note}");
    }
    for line in &service_log.recent {
        println!("  {line}");
    }

    println!(
        "\nthis fmf process — its own diagnostics ring ({}); the service's \
         diagnostics are above, not here:",
        errors.len()
    );
    println!("{}", serde_json::to_string_pretty(&errors)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A logfmt line exactly as `LogfmtFormat` writes it.
    fn line(level: &str, message: &str) -> String {
        format!("2026-09-14T12:34:56.789+09:00 {level} area=scan msg=\"{message}\"")
    }

    #[test]
    fn the_level_is_read_from_its_column_not_matched_anywhere_in_the_line() {
        assert_eq!(level_of(&line("WARN ", "x")), Some("WARN "));
        assert_eq!(level_of(&line("ERROR", "x")), Some("ERROR"));
        assert_eq!(level_of(&line("INFO ", "x")), Some("INFO "));

        assert!(is_problem(&line("WARN ", "snapshot stale")));
        assert!(is_problem(&line("ERROR", "boom")));
        assert!(!is_problem(&line("INFO ", "query served")));
        assert!(!is_problem(&line("DEBUG", "finish: entries sorted")));

        // An INFO line whose body merely says ERROR is still an INFO line. A
        // substring match would have reported it, and a file name can put that
        // word in a message body.
        assert!(!is_problem(&line("INFO ", "ERROR WARN")));

        // Fragments with no level column report none rather than guessing.
        assert_eq!(level_of(""), None);
        assert_eq!(level_of("short"), None);
        assert_eq!(level_of(&"ERROR".repeat(20)), None);
    }

    #[test]
    fn generations_are_matched_the_way_the_filesystem_names_them() {
        assert!(is_generation("engine.2026-09-14.log"));
        // NTFS is case-insensitive, so the same file can be listed under any
        // casing; a case-sensitive test would report "no log here" for a log
        // that exists, which is the silent-empty-answer failure again.
        assert!(is_generation("ENGINE.2026-09-14.LOG"));
        assert!(is_generation("Engine.2026-09-14.Log"));

        assert!(!is_generation("engine.2026-09-14.log.bak"));
        assert!(!is_generation("app.log"));
        assert!(!is_generation("engine.log.txt"));
        assert!(!is_generation("notes.txt"));
        assert!(!is_generation(""));
        // Shorter than the prefix, and multi-byte inside it: neither may panic.
        assert!(!is_generation("eng"));
        assert!(!is_generation("エンジン.log"));
    }

    #[test]
    fn an_unreadable_log_directory_is_reported_with_its_reason() {
        // The silent-empty-section failure, pinned: when there is nothing to
        // show, the output must say why there is nothing to show.
        let missing = std::env::temp_dir().join("fmf-diag-no-such-dir-8f3a1c");
        let log = read_service_log(&missing);

        assert!(log.recent.is_empty());
        assert_eq!(log.notes.len(), 1, "{:?}", log.notes);
        assert!(
            log.notes[0].contains(&missing.display().to_string()),
            "the reason must name the path: {:?}",
            log.notes
        );
        assert_eq!(log.dir, missing.display().to_string());
    }

    #[test]
    fn the_tail_reports_problem_lines_oldest_first_across_generations() {
        let dir = std::env::temp_dir().join(format!("fmf-diag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp log dir");
        std::fs::write(
            dir.join("engine.2026-09-13.log"),
            format!("{}\n{}\n", line("INFO ", "served"), line("WARN ", "older")),
        )
        .expect("write generation");
        std::fs::write(
            dir.join("engine.2026-09-14.log"),
            format!("{}\n{}\n", line("ERROR", "newer"), line("DEBUG", "noise")),
        )
        .expect("write generation");
        // Not a generation file: must be ignored, not parsed.
        std::fs::write(dir.join("notes.txt"), line("ERROR", "not a log")).expect("write stray");

        let log = read_service_log(&dir);
        let _ = std::fs::remove_dir_all(&dir);

        assert!(log.notes.is_empty(), "{:?}", log.notes);
        assert_eq!(
            log.files,
            ["engine.2026-09-13.log", "engine.2026-09-14.log"]
        );
        assert_eq!(log.recent.len(), 2, "{:?}", log.recent);
        assert!(log.recent[0].contains("older"), "{:?}", log.recent);
        assert!(log.recent[1].contains("newer"), "{:?}", log.recent);
    }
}
