//! Version pins and the pipe name. An incompatible wire change bumps the
//! pipe name itself (`-v2`), not just a number — see ARCHITECTURE.md.

// v2: FmfQueryOptions grew `regex_mode` (16→20 B) — an incompatible wire
// change, so the pipe name moves to -v2 (a stale v1 service then can't be
// reached at all, instead of decoding a 20 B request as 16 B + text;
// ADR-0023).
/// FFI ABI version — bumped when the in-process `fmf_engine.dll` POD layout
/// changes incompatibly.
pub const ABI_VERSION: u32 = 2;
/// Pipe wire protocol version — bumped when the named-pipe message format
/// changes incompatibly (which also moves the pipe name to `-v2`).
pub const PROTOCOL_VERSION: u32 = 2;

/// Full pipe path (Rust side opens this).
pub const PIPE_NAME: &str = r"\\.\pipe\fmf-engine-v2";
/// Short name (C# `NamedPipeClientStream` takes the name without the
/// `\\.\pipe\` prefix; gen-contract radiates this one).
pub const PIPE_NAME_SHORT: &str = "fmf-engine-v2";
/// SCM service name — deployment surface shared by fmf-service's
/// lifecycle subcommands and the app's in-app service setup.
pub const SERVICE_NAME: &str = "fmf-engine";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_names_agree() {
        assert_eq!(PIPE_NAME, format!(r"\\.\pipe\{PIPE_NAME_SHORT}"));
    }
}
