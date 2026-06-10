//! fmf-ffi — C ABI surface over fmf-core.
//!
//! Rules (see CLAUDE.md): no logic here, only conversion, handle management
//! and panic catching. The real API lands with milestone M1; this placeholder
//! exists so the workspace builds as a whole from day one.

/// ABI smoke probe used by early integration tests.
#[unsafe(no_mangle)]
pub extern "C" fn fmf_abi_version() -> u32 {
    1
}
