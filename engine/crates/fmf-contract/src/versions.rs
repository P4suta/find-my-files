//! Version pins and the pipe name. An incompatible wire change bumps the
//! pipe name itself (`-v2`), not just a number — see ARCHITECTURE.md.

pub const ABI_VERSION: u32 = 1;
pub const PROTOCOL_VERSION: u32 = 1;

/// Full pipe path (Rust side opens this).
pub const PIPE_NAME: &str = r"\\.\pipe\fmf-engine-v1";
/// Short name (C# `NamedPipeClientStream` takes the name without the
/// `\\.\pipe\` prefix; gen-contract radiates this one).
pub const PIPE_NAME_SHORT: &str = "fmf-engine-v1";
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
