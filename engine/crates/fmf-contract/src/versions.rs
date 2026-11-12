//! Version pins and the pipe name.
//!
//! An incompatible wire change bumps the pipe *name*, not just a number: a stale server is then unreachable instead of misreading a newer request's bytes under its old layout.
//! The history below records what each bump was for; it is append-only.

/// FFI ABI version — bumped when the in-process `fmf_engine.dll` POD layout changes incompatibly.
pub const ABI_VERSION: u32 = 5;
/// Pipe wire protocol version — bumped when the named-pipe message format changes incompatibly (which also moves the pipe name).
pub const PROTOCOL_VERSION: u32 = 4;

/// Full pipe path (Rust side opens this).
pub const PIPE_NAME: &str = r"\\.\pipe\fmf-engine-v4";
/// Short name (C# `NamedPipeClientStream` takes the name without the `\\.\pipe\` prefix; gen-contract radiates this one).
pub const PIPE_NAME_SHORT: &str = "fmf-engine-v4";
/// SCM service name — deployment surface shared by fmf-service's lifecycle subcommands and the app's in-app service setup.
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
