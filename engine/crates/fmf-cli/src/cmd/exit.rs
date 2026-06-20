//! Map a command failure to a process exit code drawn from the shared
//! `fmf_contract::codes` table, and render the error to stderr.
//!
//! The CLI consumes fmf-core directly, so it sees the same typed errors the
//! FFI/pipe boundary classifies into `FMF_E_*` codes (fmf-ffi `handle.rs`,
//! `results.rs`). We mirror that classification here so a script can branch on
//! `$LASTEXITCODE` the same way a pipe client branches on a frame status.
//! Numbers are never redefined — they come from `fmf_contract::codes`.

use std::error::Error;
use std::io::Write;

use anstream::{AutoStream, ColorChoice};
use anstyle::{AnsiColor, Style};
use fmf_contract::codes;
use fmf_core::engine::{EngineCreateError, EngineError};
use fmf_core::mft::MftError;
use fmf_core::query::{CompileError, ParseError};
use fmf_core::usn::UsnError;

/// Win32 `ERROR_ACCESS_DENIED` — a raw-volume open that fails this way means
/// the terminal is not elevated, not that the volume is unreadable.
const ERROR_ACCESS_DENIED: u32 = 5;

/// Conventional generic-failure exit code for errors the CLI cannot classify
/// (matches the Unix "general error" convention; distinct from clap's usage
/// exit code of 2, which clap sets before dispatch ever runs).
pub const GENERIC_FAILURE: i32 = 1;

/// The `FMF_E_*` status code for `err`, or `None` when it is not a recognised
/// engine error. The mapping matches the FFI boundary verbatim.
#[must_use]
pub fn classify(err: &(dyn Error + 'static)) -> Option<i32> {
    if let Some(e) = err.downcast_ref::<MftError>() {
        return Some(match e {
            MftError::NotElevated => codes::NOT_ADMIN,
            MftError::Ntfs(_) => codes::VOLUME,
        });
    }
    if let Some(e) = err.downcast_ref::<EngineCreateError>() {
        return Some(match e {
            EngineCreateError::Locked(_) => codes::LOCKED,
            EngineCreateError::Io(_) => codes::IO,
        });
    }
    if let Some(e) = err.downcast_ref::<EngineError>() {
        return Some(match e {
            EngineError::Parse(_) | EngineError::Compile(_) => codes::QUERY_SYNTAX,
            EngineError::Stale => codes::STALE,
        });
    }
    if err.is::<ParseError>() || err.is::<CompileError>() {
        return Some(codes::QUERY_SYNTAX);
    }
    if let Some(e) = err.downcast_ref::<UsnError>() {
        return Some(match e {
            UsnError::OpenVolume(_, code) if *code == ERROR_ACCESS_DENIED => codes::NOT_ADMIN,
            UsnError::OpenVolume(..) => codes::VOLUME,
            UsnError::Fsctl(_) => codes::IO,
        });
    }
    None
}

/// The process exit code for `err`: its `FMF_E_*` code, or [`GENERIC_FAILURE`].
#[must_use]
pub fn status_code(err: &(dyn Error + 'static)) -> i32 {
    classify(err).unwrap_or(GENERIC_FAILURE)
}

/// The symbolic `FMF_E_*` name for a status code, for human labels and the
/// machine-readable error payload.
#[must_use]
pub const fn code_name(code: i32) -> &'static str {
    match code {
        codes::OK => "FMF_OK",
        codes::INVALID_ARG => "FMF_E_INVALID_ARG",
        codes::STALE => "FMF_E_STALE",
        codes::NOT_ADMIN => "FMF_E_NOT_ADMIN",
        codes::VOLUME => "FMF_E_VOLUME",
        codes::QUERY_SYNTAX => "FMF_E_QUERY_SYNTAX",
        codes::IO => "FMF_E_IO",
        codes::LOCKED => "FMF_E_LOCKED",
        codes::PANIC => "FMF_E_PANIC",
        _ => "FMF_E_UNKNOWN",
    }
}

/// Print `err` and its cause chain to stderr (red `error[CODE]:` label when the
/// stream takes colour) and return the process exit code. A classified error is
/// labelled with its `FMF_E_*` code; an unclassified one is a plain `error:`.
#[must_use]
pub fn report(err: &(dyn Error + 'static), color: ColorChoice) -> i32 {
    let mut stderr = AutoStream::new(std::io::stderr(), color);
    let red = Style::new().fg_color(Some(AnsiColor::Red.into())).bold();
    let label = match classify(err) {
        Some(code) => format!("error[{}]", code_name(code)),
        None => "error".to_owned(),
    };
    // anstream strips these escapes when the stream is not taking colour.
    let _ = writeln!(
        stderr,
        "{}{label}{}: {err}",
        red.render(),
        red.render_reset()
    );
    let mut source = err.source();
    while let Some(cause) = source {
        let _ = writeln!(stderr, "  caused by: {cause}");
        source = cause.source();
    }
    status_code(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(e: impl Error + 'static) -> Box<dyn Error> {
        Box::new(e)
    }

    #[test]
    fn known_engine_errors_match_the_ffi_table() {
        assert_eq!(
            status_code(boxed(MftError::NotElevated).as_ref()),
            codes::NOT_ADMIN
        );
        assert_eq!(
            status_code(boxed(EngineCreateError::Locked(None)).as_ref()),
            codes::LOCKED
        );
        assert_eq!(
            status_code(boxed(EngineCreateError::Io(std::io::Error::other("x"))).as_ref()),
            codes::IO
        );
        assert_eq!(
            status_code(boxed(EngineError::Stale).as_ref()),
            codes::STALE
        );
    }

    #[test]
    fn parse_errors_are_query_syntax() {
        // `"` opens a quote that never closes — a guaranteed ParseError.
        let err = fmf_core::query::parse("\"").unwrap_err();
        assert_eq!(status_code(boxed(err).as_ref()), codes::QUERY_SYNTAX);
    }

    #[test]
    fn usn_open_access_denied_is_not_admin() {
        let denied = UsnError::OpenVolume(r"\\.\C:".into(), ERROR_ACCESS_DENIED);
        assert_eq!(status_code(boxed(denied).as_ref()), codes::NOT_ADMIN);
        let other = UsnError::OpenVolume(r"\\.\C:".into(), 2);
        assert_eq!(status_code(boxed(other).as_ref()), codes::VOLUME);
    }

    #[test]
    fn unclassified_errors_are_generic() {
        let err: Box<dyn Error> = "boom".into();
        assert_eq!(classify(err.as_ref()), None);
        assert_eq!(status_code(err.as_ref()), GENERIC_FAILURE);
    }

    #[test]
    fn code_names_cover_the_contract_table() {
        assert_eq!(code_name(codes::LOCKED), "FMF_E_LOCKED");
        assert_eq!(code_name(codes::NOT_ADMIN), "FMF_E_NOT_ADMIN");
        assert_eq!(code_name(codes::PANIC), "FMF_E_PANIC");
        assert_eq!(code_name(-1), "FMF_E_UNKNOWN");
    }
}
