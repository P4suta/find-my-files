//! Terminal presentation shared by the CLI's human-facing output: the
//! `--color` choice and how it resolves against the `NO_COLOR` /
//! `CLICOLOR_FORCE` conventions and TTY detection. The actual stripping of
//! ANSI when a stream is redirected is anstream's job — nothing here writes
//! escape codes itself.

use anstream::ColorChoice;
use clap::ValueEnum;

/// The `--color` mode requested on the command line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum ColorArg {
    /// Colour when the stream is a terminal and the environment allows it.
    #[default]
    Auto,
    /// Always emit colour, even when redirected.
    Always,
    /// Never emit colour.
    Never,
}

/// Resolve `--color` into anstream's [`ColorChoice`].
///
/// `always`/`never` are explicit overrides. `auto` honours the `NO_COLOR` and
/// `CLICOLOR_FORCE` conventions and otherwise defers to the stream's own TTY
/// detection ([`ColorChoice::Auto`]).
#[must_use]
pub fn resolve_color(arg: ColorArg) -> ColorChoice {
    match arg {
        ColorArg::Always => ColorChoice::Always,
        ColorArg::Never => ColorChoice::Never,
        ColorArg::Auto => {
            // NO_COLOR: any non-empty value disables colour (no-color.org).
            if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
                ColorChoice::Never
            // CLICOLOR_FORCE: any non-empty value other than "0" forces colour.
            } else if std::env::var_os("CLICOLOR_FORCE").is_some_and(|v| !v.is_empty() && v != "0")
            {
                ColorChoice::Always
            } else {
                ColorChoice::Auto
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_modes_ignore_environment() {
        assert!(matches!(
            resolve_color(ColorArg::Always),
            ColorChoice::Always
        ));
        assert!(matches!(resolve_color(ColorArg::Never), ColorChoice::Never));
    }
}
