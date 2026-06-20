//! Terminal presentation shared by the CLI's human-facing output: the
//! `--color` choice and how it resolves against the `NO_COLOR` /
//! `CLICOLOR_FORCE` conventions and TTY detection. The actual stripping of
//! ANSI when a stream is redirected is anstream's job — nothing here writes
//! escape codes itself.

use anstream::ColorChoice;
use anstyle::{AnsiColor, Color, Effects, Style};
use clap::ValueEnum;
use indicatif::{ProgressBar, ProgressStyle};

use super::ctx::Ctx;

/// A failure or regression label (red, bold).
pub const ERROR: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Red)))
    .effects(Effects::BOLD);
/// A recoverable warning label (yellow, bold).
pub const WARN: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Yellow)))
    .effects(Effects::BOLD);
/// A success / within-budget label (green, bold).
pub const OK: Style = Style::new()
    .fg_color(Some(Color::Ansi(AnsiColor::Green)))
    .effects(Effects::BOLD);
/// A table header or section title (bold).
pub const HEADER: Style = Style::new().effects(Effects::BOLD);

/// Wrap `text` in `style`'s ANSI escapes. The result is meant to be written
/// through an anstream sink (e.g. `anstream::println!`), which strips the
/// escapes again when the stream is not taking colour — so callers never have
/// to know whether colour is on.
#[must_use]
pub fn paint(style: Style, text: &str) -> String {
    format!("{}{text}{}", style.render(), style.render_reset())
}

/// A steady-ticking spinner for a long blocking step (e.g. indexing a volume).
///
/// Returns a hidden, inert bar when `--quiet` is set or stderr is not a
/// terminal, so the spinner never lands in piped or redirected output. Call
/// [`ProgressBar::finish_and_clear`] when the step ends.
#[must_use]
pub fn spinner(ctx: Ctx, message: impl Into<std::borrow::Cow<'static, str>>) -> ProgressBar {
    use std::io::IsTerminal as _;
    if ctx.quiet || !std::io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner} {msg} ({elapsed})")
            .expect("static spinner template is valid"),
    );
    pb.set_message(message);
    pb.enable_steady_tick(std::time::Duration::from_millis(120));
    pb
}

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
