use anyhow::Result;
use colored::Colorize;

use crate::commands::ctx::Ctx;
use crate::utils::containers::{fetch_containers, state_icon};

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;
    let containers = fetch_containers(&ctx.rt, verbose)?;

    ctx.out.section("health", || {
        if containers.is_empty() {
            println!("  {}", "No containers found".dimmed());
            return;
        }
        for c in &containers {
            let health_str = match c.health.as_str() {
                "healthy" => "healthy".green().to_string(),
                "unhealthy" => "unhealthy".red().bold().to_string(),
                "starting" => "starting…".yellow().to_string(),
                _ => "no check".dimmed().to_string(),
            };
            println!(
                "  {:<20} {} {:<12}  {}",
                c.service.bold(),
                state_icon(&c.state),
                c.state.dimmed(),
                health_str,
            );
        }
    });
    Ok(())
}
