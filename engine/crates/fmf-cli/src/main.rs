//! Binary entry point for the `fmf` developer CLI. All logic lives in the
//! `fmf_cli` library so tests and the completion command reuse one clap
//! surface.

fn main() {
    fmf_cli::run();
}
