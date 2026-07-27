//! Version pins and the pipe name.
//!
//! An incompatible wire change bumps the pipe *name*, not just a number: a
//! stale server is then unreachable instead of misreading a newer request's
//! bytes under its old layout. The history below records what each bump was
//! for; it is append-only.

// v2: FmfQueryOptions grew `regex_mode` (16→20 B) — an incompatible wire
// change, so the pipe name moves to -v2 (a stale v1 service then can't be
// reached at all, instead of decoding a 20 B request as 16 B + text;
// ADR-0023).
// v3: FmfRow widened both WTF-8 byte lengths from u16 to u32 (48→56 B)
// so every valid NTFS path is representable. The reserved tail word is zero.
// v4: the FFI-only FmfPage/FmfBlob descriptors gained monotonic owner_id
// fields and their free functions changed from address ownership to ID
// ownership. The pipe row/blob wire layout remains v3.
// v5/v4: queries gained an optional presentation-basis result ID and
// cooperative cancellation. This changes the FFI ABI (query controls and
// FmfQueryOptions) and the pipe wire (32-byte options plus QueryCancel), so
// the ABI becomes 5 and the named-pipe protocol moves to v4 (ADR-0044).
/// FFI ABI version — bumped when the in-process `fmf_engine.dll` POD layout
/// changes incompatibly.
pub const ABI_VERSION: u32 = 5;
/// Pipe wire protocol version — bumped when the named-pipe message format
/// changes incompatibly (which also moves the pipe name).
pub const PROTOCOL_VERSION: u32 = 4;

/// Full pipe path (Rust side opens this).
pub const PIPE_NAME: &str = r"\\.\pipe\fmf-engine-v4";
/// Short name (C# `NamedPipeClientStream` takes the name without the
/// `\\.\pipe\` prefix; gen-contract radiates this one).
pub const PIPE_NAME_SHORT: &str = "fmf-engine-v4";
/// SCM service name — deployment surface shared by fmf-service's
/// lifecycle subcommands and the app's in-app service setup.
pub const SERVICE_NAME: &str = "fmf-engine";
/// Exact SCM Description required before the UI starts a stopped service.
///
/// An absent or different marker requires re-registration.
pub const SERVICE_PROTOCOL_MARKER: &str =
    "find-my-files filename index engine; protocol=4; pipe=fmf-engine-v4";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_names_agree() {
        assert_eq!(PIPE_NAME, format!(r"\\.\pipe\{PIPE_NAME_SHORT}"));
        assert!(SERVICE_PROTOCOL_MARKER.contains(PIPE_NAME_SHORT));
        assert!(SERVICE_PROTOCOL_MARKER.contains(&format!("protocol={PROTOCOL_VERSION}")));
    }
}
