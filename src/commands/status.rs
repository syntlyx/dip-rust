use std::time::Duration;

use anyhow::Result;
use colored::Colorize;

use crate::commands::ctx::Ctx;
use crate::utils::containers::{fetch_containers, state_icon};
use crate::utils::output::{format_ports, service_color};

pub fn run(
    format: Option<&str>,
    watch: bool,
    interval: u64,
    verbose: bool,
    no_color: bool,
) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;
    let project_name = ctx.rt.project.project_name.clone();

    if format == Some("json") {
        let containers = fetch_containers(&ctx.rt, verbose)?;
        let json = serde_json::json!({
            "project": project_name,
            "containers": containers,
        });
        println!("{}", serde_json::to_string_pretty(&json)?);
        return Ok(());
    }

    if watch {
        let delay = Duration::from_secs(interval);
        loop {
            // Move cursor to top-left and clear screen for in-place refresh.
            print!("\x1b[H\x1b[2J");
            let now = chrono::Local::now().format("%H:%M:%S");
            println!(
                "  {} {}s  {}",
                "watch".dimmed(),
                interval,
                now.to_string().dimmed()
            );
            // Ignore errors mid-loop — containers may be momentarily unreachable.
            let _ = print_table(&ctx, &project_name, verbose);
            std::thread::sleep(delay);
        }
    }

    print_table(&ctx, &project_name, verbose)
}

// ─── rendering ────────────────────────────────────────────────────────────────

fn print_table(ctx: &Ctx, project_name: &str, verbose: bool) -> Result<()> {
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

        let uptime_str = if c.state == "running" {
            c.status
                .strip_prefix("Up ")
                .map(|u| format!("  {}", u.trim().dimmed()))
                .unwrap_or_default()
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
