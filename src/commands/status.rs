use anyhow::Result;
use colored::Colorize;

use crate::commands::ctx::Ctx;
use crate::utils::containers::{fetch_containers, state_icon};
use crate::utils::output::{format_ports, service_color};

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;
    let project_name = ctx.rt.project.project_name.clone();
    let containers = fetch_containers(&ctx.rt, verbose)?;

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
        ctx.out.info("No containers found");
        println!("{}", "─".repeat(width));
        return Ok(());
    }

    for c in &containers {
        let color = service_color(&c.service);
        let service_col = format!("{:>col_w$}", c.service).color(color).bold();

        let health = match c.health.as_str() {
            "healthy" => "  healthy".green().dimmed().to_string(),
            "unhealthy" => "  unhealthy".red().to_string(),
            "starting" => "  starting…".yellow().dimmed().to_string(),
            _ => String::new(),
        };

        let ports = format_ports(&c.ports);
        let ports_str = if ports.is_empty() {
            String::new()
        } else {
            format!("  {}", ports.dimmed())
        };

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
            state_icon(&c.state),
            c.state.dimmed(),
            uptime_str,
            health,
            ports_str,
        );
    }

    println!("{}", "─".repeat(width));
    Ok(())
}
