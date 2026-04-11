use anyhow::Result;
use colored::Colorize;
use serde::Deserialize;

use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::output::{Output, format_ports, service_color};
use crate::utils::parse_jsonl;

#[derive(Deserialize)]
struct ContainerRow {
    #[serde(rename = "Service")]
    service: String,
    #[serde(rename = "State")]
    state: String,
    #[serde(rename = "Status")]
    status: String,
    #[serde(rename = "Health")]
    health: Option<String>,
    #[serde(rename = "Ports")]
    ports: String,
}

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    let project = ProjectConfig::load()?;
    let project_name = project.project_name.clone();
    let rt = Runtime::new(project, verbose, no_color);

    let raw = rt.compose_capture(&["ps", "-a", "--format", "json"])?;
    let containers: Vec<ContainerRow> = parse_jsonl(&raw, verbose);

    let col_w = containers
        .iter()
        .map(|c| c.service.len())
        .max()
        .unwrap_or(8)
        .max(8);
    let width = col_w + 40;

    println!("{}", "─".repeat(width));
    println!("  {} {}", "project".dimmed(), project_name.bold());
    println!("{}", "─".repeat(width));

    if containers.is_empty() {
        out.info("No containers found");
        println!("{}", "─".repeat(width));
        return Ok(());
    }

    for c in &containers {
        let color = service_color(&c.service);
        let service_col = format!("{:>col_w$}", c.service).color(color).bold();

        let state_icon = match c.state.as_str() {
            "running" => "●".green().bold(),
            "exited" | "dead" => "●".red(),
            "paused" => "●".yellow(),
            _ => "●".dimmed(),
        };

        let health = match c.health.as_deref().unwrap_or("") {
            "healthy" => "  healthy".green().dimmed().to_string(),
            "unhealthy" => "  unhealthy".red().to_string(),
            "starting" => "  starting…".yellow().dimmed().to_string(),
            _ => String::new(),
        };

        // Compact ports: "0.0.0.0:8080->80/tcp" → "8080→80"
        let ports = format_ports(&c.ports);
        let ports_str = if ports.is_empty() {
            String::new()
        } else {
            format!("  {}", ports.dimmed())
        };

        // Status uptime part: "Up 2 hours" → "2 hours"
        let uptime = c.status.strip_prefix("Up ").unwrap_or("").trim();
        let uptime_str = if !uptime.is_empty() && c.state == "running" {
            format!("  {}", uptime.dimmed())
        } else if c.state == "exited" {
            format!("  {}", c.status.dimmed())
        } else {
            String::new()
        };

        println!(
            "  {} {} {}{}{}{}",
            service_col,
            state_icon,
            c.state.dimmed(),
            uptime_str,
            health,
            ports_str,
        );
    }

    println!("{}", "─".repeat(width));
    Ok(())
}
