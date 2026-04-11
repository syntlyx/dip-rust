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

    out.section("health", || {
        if containers.is_empty() {
            println!("  {}", "No containers found".dimmed());
            return;
        }
        for c in &containers {
            let state_icon = match c.state.as_str() {
                "running" => "●".green().bold(),
                "exited" | "dead" => "●".red(),
                "paused" => "●".yellow(),
                _ => "●".dimmed(),
            };
            let health_str = match c.health.as_str() {
                "healthy" => "healthy".green().to_string(),
                "unhealthy" => "unhealthy".red().bold().to_string(),
                "starting" => "starting…".yellow().to_string(),
                _ => "no check".dimmed().to_string(),
            };
            println!(
                "  {:<20} {} {:<12}  {}",
                c.service.bold(),
                state_icon,
                c.state.dimmed(),
                health_str,
            );
        }
    });
    Ok(())
}
