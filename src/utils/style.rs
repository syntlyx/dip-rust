//! Minimal ANSI styling, replacing the `colored` crate.
//!
//! Only the colors and attributes actually used by this project are
//! implemented. The [`Stylize`] extension trait mirrors the method names of
//! `colored::Colorize`, so call sites only need to swap the import line:
//!
//! ```text
//! use crate::utils::style::Stylize;
//!
//! println!("{}", "done".green().bold());
//! ```
//!
//! Whether escape codes are emitted is decided at `Display` time by a global
//! switch (see [`set_override`]), matching the behavior of
//! `colored::control::set_override`.

use std::fmt;
use std::io::IsTerminal;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

// ─── global color control ─────────────────────────────────────────────────────

const UNSET: u8 = 0;
const FORCE_OFF: u8 = 1;
const FORCE_ON: u8 = 2;

/// Explicit override set by `set_override`; `UNSET` falls back to the
/// environment-based default.
static OVERRIDE: AtomicU8 = AtomicU8::new(UNSET);

/// Lazily computed environment default (`NO_COLOR` + stdout TTY check).
static ENV_DEFAULT: OnceLock<bool> = OnceLock::new();

/// Force colors on or off globally, overriding the environment default.
/// Same semantics as `colored::control::set_override`.
pub fn set_override(enabled: bool) {
    let value = if enabled { FORCE_ON } else { FORCE_OFF };
    OVERRIDE.store(value, Ordering::Relaxed);
}

/// Whether escape codes should be emitted right now.
///
/// An explicit override always wins; otherwise colors are on when the
/// `NO_COLOR` env var is absent and stdout is a terminal.
fn colors_enabled() -> bool {
    match OVERRIDE.load(Ordering::Relaxed) {
        FORCE_OFF => false,
        FORCE_ON => true,
        _ => *ENV_DEFAULT.get_or_init(|| {
            std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
        }),
    }
}

// ─── colors ───────────────────────────────────────────────────────────────────

/// Foreground terminal colors used by this project.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    BrightGreen,
    BrightBlue,
    BrightMagenta,
    BrightCyan,
}

impl Color {
    /// SGR foreground code for this color.
    fn code(self) -> &'static str {
        match self {
            Color::Red => "31",
            Color::Green => "32",
            Color::Yellow => "33",
            Color::Blue => "34",
            Color::Magenta => "35",
            Color::Cyan => "36",
            Color::BrightGreen => "92",
            Color::BrightBlue => "94",
            Color::BrightMagenta => "95",
            Color::BrightCyan => "96",
        }
    }
}

// ─── styled text ──────────────────────────────────────────────────────────────

/// A piece of text plus the styles to apply when displayed.
///
/// Escape codes are only emitted by the `Display` impl, and only when colors
/// are globally enabled — so a `Styled` value can be built unconditionally
/// and still print plain text under `--no-color`.
pub struct Styled {
    text: String,
    fg: Option<Color>,
    bold: bool,
    dimmed: bool,
}

impl Styled {
    fn new(text: String) -> Self {
        Self {
            text,
            fg: None,
            bold: false,
            dimmed: false,
        }
    }

    /// True when no styles are set, so no escape codes are needed.
    fn is_plain(&self) -> bool {
        self.fg.is_none() && !self.bold && !self.dimmed
    }
}

impl fmt::Display for Styled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_plain() || !colors_enabled() {
            // Delegate so format specs like `{:<10}` keep working on plain
            // text (escape codes would break width calculations anyway).
            return fmt::Display::fmt(&self.text, f);
        }
        // Accumulate all SGR codes into a single escape sequence:
        // `ESC[<bold>;<dim>;<fg>m text ESC[0m`.
        let mut codes: Vec<&str> = Vec::with_capacity(3);
        if self.bold {
            codes.push("1");
        }
        if self.dimmed {
            codes.push("2");
        }
        if let Some(fg) = self.fg {
            codes.push(fg.code());
        }
        write!(f, "\x1b[{}m{}\x1b[0m", codes.join(";"), self.text)
    }
}

// ─── extension trait ──────────────────────────────────────────────────────────

/// Extension trait mirroring `colored::Colorize` for the methods this
/// project uses. Implemented for `&str` (which also covers `String` and
/// `&String` receivers via deref, exactly like `colored`) and for `Styled`
/// itself, so calls chain: `"hi".red().bold()`.
pub trait Stylize: Sized {
    /// Wrap the value in a [`Styled`] without applying any style.
    fn styled(self) -> Styled;

    fn red(self) -> Styled {
        self.color(Color::Red)
    }

    fn green(self) -> Styled {
        self.color(Color::Green)
    }

    fn yellow(self) -> Styled {
        self.color(Color::Yellow)
    }

    fn blue(self) -> Styled {
        self.color(Color::Blue)
    }

    fn cyan(self) -> Styled {
        self.color(Color::Cyan)
    }

    fn color(self, color: Color) -> Styled {
        let mut s = self.styled();
        s.fg = Some(color);
        s
    }

    fn bold(self) -> Styled {
        let mut s = self.styled();
        s.bold = true;
        s
    }

    fn dimmed(self) -> Styled {
        let mut s = self.styled();
        s.dimmed = true;
        s
    }

    /// No-op styling; useful when one match arm needs a `Styled` value
    /// without any color applied.
    fn normal(self) -> Styled {
        self.styled()
    }
}

impl Stylize for &str {
    fn styled(self) -> Styled {
        Styled::new(self.to_string())
    }
}

impl Stylize for Styled {
    fn styled(self) -> Styled {
        self
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// All tests mutate the global override, so they are serialized through
    /// this lock to stay independent of test-runner threading.
    static COLOR_LOCK: Mutex<()> = Mutex::new(());

    fn with_colors<R>(enabled: bool, f: impl FnOnce() -> R) -> R {
        let _guard = COLOR_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_override(enabled);
        f()
    }

    #[test]
    fn renders_escape_codes_when_colors_enabled() {
        with_colors(true, || {
            assert_eq!("err".red().to_string(), "\x1b[31merr\x1b[0m");
            assert_eq!("ok".green().to_string(), "\x1b[32mok\x1b[0m");
            assert_eq!("note".dimmed().to_string(), "\x1b[2mnote\x1b[0m");
        });
    }

    #[test]
    fn renders_plain_text_when_colors_disabled() {
        with_colors(false, || {
            assert_eq!("err".red().bold().to_string(), "err");
            assert_eq!("note".dimmed().to_string(), "note");
        });
    }

    #[test]
    fn chained_styles_emit_a_single_escape_sequence() {
        with_colors(true, || {
            let s = "hi".red().bold().to_string();
            assert_eq!(s, "\x1b[1;31mhi\x1b[0m");
            assert_eq!(s.matches('\x1b').count(), 2, "one open + one reset");
        });
    }

    #[test]
    fn normal_produces_no_escape_codes() {
        with_colors(true, || {
            assert_eq!("plain".normal().to_string(), "plain");
        });
    }

    #[test]
    fn color_method_applies_palette_colors() {
        with_colors(true, || {
            assert_eq!(
                "svc".color(Color::BrightCyan).bold().to_string(),
                "\x1b[1;96msvc\x1b[0m"
            );
        });
    }

    #[test]
    fn set_override_toggles_rendering() {
        let _guard = COLOR_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_override(true);
        assert_eq!("x".cyan().to_string(), "\x1b[36mx\x1b[0m");
        set_override(false);
        assert_eq!("x".cyan().to_string(), "x");
    }

    #[test]
    fn works_on_owned_strings() {
        with_colors(true, || {
            let owned = String::from("own");
            assert_eq!(owned.yellow().to_string(), "\x1b[33mown\x1b[0m");
        });
    }

    #[test]
    fn plain_styled_honors_width_formatting() {
        with_colors(true, || {
            // Same quirk as `colored`: padding works for plain values only.
            assert_eq!(format!("{:<6}|", "ab".normal()), "ab    |");
        });
    }
}
