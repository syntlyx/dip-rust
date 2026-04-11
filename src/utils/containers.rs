//! Shared container types and helpers used by status, health, and other commands.

use anyhow::Result;
use colored::{ColoredString, Colorize};
use serde::Deserialize;

use crate::runtime::Runtime;

// ─── shared row type ──────────────────────────────────────────────────────────

/// One row from `docker compose ps -a --format json`.
///
/// All non-essential fields are `Option` so both `status` and `health` commands
/// can deserialise the same JSON without needing separate structs.
#[derive(Deserialize)]
pub struct ContainerRow {
    #[serde(rename = "Service")]
    pub service: String,
    #[serde(rename = "State")]
    pub state: String,
    /// Human-readable status string, e.g. "Up 2 hours" or "Exited (1) 3 minutes ago"
    #[serde(rename = "Status", default)]
    pub status: String,
    /// Present only when a HEALTHCHECK is configured
    #[serde(rename = "Health", default)]
    pub health: String,
    /// Raw port mapping string, e.g. "0.0.0.0:8080->80/tcp"
    #[serde(rename = "Ports", default)]
    pub ports: String,
}

// ─── helpers ──────────────────────────────────────────────────────────────────

/// Fetch all containers for the current project (including stopped ones).
pub fn fetch_containers(rt: &Runtime, verbose: bool) -> Result<Vec<ContainerRow>> {
    let raw = rt.compose_capture(&["ps", "-a", "--format", "json"])?;
    Ok(crate::utils::parse_jsonl(&raw, verbose))
}

/// Map a container state string to a coloured bullet icon.
///
/// ```text
/// running  → green  ●
/// exited   → red    ●
/// dead     → red    ●
/// paused   → yellow ●
/// _        → dimmed ●
/// ```
pub fn state_icon(state: &str) -> ColoredString {
    match state {
        "running" => "●".green().bold(),
        "exited" | "dead" => "●".red(),
        "paused" => "●".yellow(),
        _ => "●".dimmed(),
    }
}
