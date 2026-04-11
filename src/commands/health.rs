use anyhow::Result;
use colored::Colorize;
use serde::Deserialize;

use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::output::Output;
use crate::utils::parse_jsonl;

#[derive(Deserialize)]
struct ContainerRow {
    #[serde(rename = "Service")]
    service: String,
    #[serde(rename = "State")]
    state: String,
    #[serde(rename = "Health")]
    health: String,
}

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    let project = ProjectConfig::load()?;
    let rt = Runtime::new(project, verbose, no_color);

    let raw = rt.compose_capture(&["ps", "-a", "--format", "json"])?;

    let containers: Vec<ContainerRow> = parse_jsonl(&raw, verbose);

    if containers.is_empty() {
        out.info("No containers found for this project");
        return Ok(());
    }

    println!(
        "{:<20} {:<14} {:<14}",
        "SERVICE".bold(),
        "STATE".bold(),
        "HEALTH".bold()
    );
    println!("{}", "─".repeat(50));

    for c in &containers {
        let state_col = match c.state.as_str() {
            "running" => c.state.green().to_string(),
            "exited" | "dead" => c.state.red().to_string(),
            "paused" => c.state.yellow().to_string(),
            _ => c.state.clone(),
        };
        let health_col = match c.health.as_str() {
            "healthy" => c.health.green().to_string(),
            "unhealthy" => c.health.red().to_string(),
            "starting" => c.health.yellow().to_string(),
            "" | "none" => "no check".dimmed().to_string(),
            _ => c.health.clone(),
        };
        println!("{:<20} {:<14} {:<14}", c.service, state_col, health_col);
    }

    Ok(())
}
