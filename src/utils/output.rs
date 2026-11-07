use crate::utils::spinner::Spinner;
use crate::utils::style::{Color, Stylize};

pub struct Output;

impl Output {
    pub fn new(no_color: bool) -> Self {
        // Only `--no-color` overrides. Forcing colors *on* here (which is what
        // passing `!no_color` did) made the environment default dead code, so
        // `NO_COLOR` was ignored and `dip status > report.txt` wrote escape
        // codes into the file.
        if no_color {
            crate::utils::style::disable();
        }
        Self
    }

    // Progress, confirmations, warnings and errors are diagnostics: they say
    // what dip is doing, not what the command was asked to produce. They go to
    // stderr so stdout carries only the payload — `dip exec app pg_dump | gzip`
    // and `dip status --format json | jq` have to see nothing else. An
    // interactive user sees both streams on the terminal either way.
    //
    // Use `report` for a line that *is* the answer, and `section`/`println!`
    // for rendered report bodies; both stay on stdout.

    pub fn success(&self, message: &str) {
        eprintln!("{} {}", "✓".green().bold(), message);
    }

    pub fn info(&self, message: &str) {
        eprintln!("{} {}", "·".blue(), message.dimmed());
    }

    pub fn warning(&self, message: &str) {
        eprintln!("{} {}", "⚠".yellow(), message.yellow());
    }

    pub fn error(&self, message: &str) {
        eprintln!("{} {}", "✗".red().bold(), message.red());
    }

    /// A line that *is* the command's answer rather than commentary about it.
    ///
    /// `dip stats` with nothing running still has an answer — "no running
    /// containers" — and it belongs on stdout with the rest of the report, so
    /// that `dip stats > file` records it and a table does not end up framed
    /// around a gap. Renders like `info`; only the stream differs.
    pub fn report(&self, message: &str) {
        println!("{} {}", "·".blue(), message.dimmed());
    }

    /// Print a bordered section with a title and body.
    /// Width adapts to the current terminal width (falls back to 80).
    ///
    /// ```text
    /// ────────────────────────────────────
    ///   title
    /// ────────────────────────────────────
    ///   <body printed by closure>
    /// ────────────────────────────────────
    /// ```
    pub fn section(&self, title: &str, body: impl FnOnce()) {
        let width = term_width();
        let sep = "─".repeat(width);
        println!("{sep}");
        println!("  {}", title.bold().green());
        println!("{sep}");
        body();
        println!("{sep}");
    }

    pub fn spinner(&self, message: &str) -> Spinner {
        make_spinner(message)
    }
}

/// Create a spinner.
///
/// Color is controlled by the global `style` override set in `Output::new` —
/// no need to pass `no_color` here separately.
pub fn make_spinner(message: &str) -> Spinner {
    Spinner::new(message)
}

// ─── shared formatting helpers ────────────────────────────────────────────────

/// Colorize a line from `docker compose` output.
///
/// Docker Compose prefixes each log line with `"service_name  | rest"`.
/// This function right-pads the service name to a fixed width and applies a
/// deterministic per-service color so parallel container output is easy to
/// scan, matching the style of `docker compose logs`.
pub fn colorize_compose_line(line: &str, no_color: bool) -> String {
    if no_color {
        return line.to_string();
    }
    if let Some((service, rest)) = line.split_once(" | ") {
        format!("{} {} {}", pad_service(service.trim()), "|".dimmed(), rest)
    } else {
        line.dimmed().to_string()
    }
}

fn pad_service(name: &str) -> impl std::fmt::Display + '_ {
    let color = service_color(name);
    // Right-align in a fixed 12-char column so the pipe stays in one column
    format!("{:>12}", name).color(color).bold()
}

/// Compact port mapping: "0.0.0.0:8080->80/tcp, :::8080->80/tcp" → "8080→80"
pub fn format_ports(raw: &str) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut parts = Vec::new();
    for mapping in raw.split(", ") {
        if let Some(arrow) = mapping.find("->") {
            let left = &mapping[..arrow];
            let right = &mapping[arrow + 2..];
            let host_port = left.split(':').next_back().unwrap_or(left);
            let container_port = right.split('/').next().unwrap_or(right);
            let key = format!("{host_port}→{container_port}");
            if seen.insert(key.clone()) {
                parts.push(key);
            }
        }
    }
    parts.join("  ")
}

/// Current terminal width, falling back to 80 if not a tty (e.g. pipes, CI).
pub fn term_width() -> usize {
    #[cfg(unix)]
    unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
            return ws.ws_col as usize;
        }
    }
    80
}

/// Map a service name to a stable terminal color.
pub fn service_color(name: &str) -> Color {
    const PALETTE: &[Color] = &[
        Color::Cyan,
        Color::Green,
        Color::Magenta,
        Color::Blue,
        Color::BrightCyan,
        Color::BrightGreen,
        Color::BrightMagenta,
        Color::BrightBlue,
    ];
    let hash = name
        .bytes()
        .fold(0usize, |acc, b| acc.wrapping_add(b as usize));
    PALETTE[hash % PALETTE.len()]
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    /// Guard the stdout/stderr split at the source level.
    ///
    /// The split is only enforceable by reading the code: these helpers write
    /// to the process's real streams, which a unit test cannot capture without
    /// redirecting fds for the whole test binary. Asserting on the macro each
    /// helper uses is the cheap way to keep a future edit from quietly putting
    /// progress back on stdout, where it lands in the middle of piped data.
    #[test]
    fn diagnostics_go_to_stderr_and_payload_to_stdout() {
        let src = include_str!("output.rs");

        for (name, macro_) in [
            // Commentary about what dip is doing.
            ("success", "eprintln!"),
            ("info", "eprintln!"),
            ("warning", "eprintln!"),
            ("error", "eprintln!"),
            // The command's actual answer.
            ("report", "println!"),
        ] {
            let body = fn_body(src, name);
            assert!(
                body.contains(macro_),
                "Output::{name} must write with {macro_}, got:\n{body}"
            );
        }
    }

    /// Source text of `pub fn <name>` up to the next blank line.
    fn fn_body<'a>(src: &'a str, name: &str) -> &'a str {
        let needle = format!("pub fn {name}(&self, message: &str) {{");
        let start = src
            .find(&needle)
            .unwrap_or_else(|| panic!("Output::{name} not found"));
        let rest = &src[start..];
        let end = rest.find("\n\n").unwrap_or(rest.len());
        &rest[..end]
    }
}
