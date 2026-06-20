//! Binary entry point for the `fmf` developer CLI. All logic lives in the
//! `fmf_cli` library so the example codegen and the integration tests can
//! reuse the same clap surface.

fn main() {
    fmf_cli::run();
}
