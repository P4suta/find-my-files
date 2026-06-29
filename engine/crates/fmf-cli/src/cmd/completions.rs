//! `completions` — print a shell completion script to stdout (the gh/rustup
//! pattern: `eval "$(fmf completions bash)"`). The release bundle also ships
//! pre-generated scripts under `completions/`, produced by this same path so the
//! two never drift.

use std::io;

use clap_complete::Shell;

/// Write the completion script for `shell` to stdout. Renders from the single
/// clap command tree (`crate::command`) so it always matches the real argv parser.
pub fn completions(shell: Shell) {
    let mut command = crate::command();
    clap_complete::generate(shell, &mut command, "fmf", &mut io::stdout());
}
