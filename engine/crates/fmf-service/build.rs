//! MSVC linker policy for the elevated service executable.

use std::env;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // The service executable is launched through UAC from the extracted,
    // user-writable bundle before it is copied into ProgramData. Restrict its
    // statically imported DLLs to System32 so an adjacent planted DLL cannot
    // cross that elevation boundary.
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg-bin=fmf-service=/DEPENDENTLOADFLAG:0x800");
    }
}
