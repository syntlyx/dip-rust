use colored::{Color, Colorize};
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub struct Output;

impl Output {
    pub fn new(no_color: bool) -> Self {
        colored::control::set_override(!no_color);
        Self
    }

    pub fn success(&self, message: &str) {
        println!("{} {}", "✓".green().bold(), message);
    }

    pub fn info(&self, message: &str) {
        println!("{} {}", "·".blue(), message.dimmed());
    }

    pub fn warning(&self, message: &str) {
        println!("{} {}", "⚠".yellow(), message.yellow());
    }

    pub fn error(&self, message: &str) {
        println!("{} {}", "✗".red().bold(), message.red());
    }

    pub fn spinner(&self, message: &str) -> ProgressBar {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap(),
        );
        pb.set_message(message.to_string());
        pb.enable_steady_tick(Duration::from_millis(100));
        pb
    }
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
