use anyhow::Result;
use colored::Colorize;

use crate::commands::ctx::Ctx;
use crate::utils::output::service_color;

pub fn run(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;

    let header = match &service {
        Some(s) => format!("  {} {}", "logs".dimmed(), s.color(service_color(s)).bold()),
        None => format!(
            "  {} {}",
            "logs".dimmed(),
            ctx.rt.project.project_name.bold()
        ),
    };
    println!("{}", "─".repeat(50));
    println!("{header}");
    println!("{}", "─".repeat(50));

    // Show available services if no specific one requested.
    // Clippy requires collapsing the two `if` into one with `&& let`.
    #[allow(clippy::collapsible_if)]
    if service.is_none() && !no_color {
        if let Ok(raw) = ctx.rt.compose_capture(&["config", "--services"]) {
            let services: Vec<&str> = raw
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            if !services.is_empty() {
                let colored: Vec<String> = services
                    .iter()
                    .map(|s| s.color(service_color(s)).bold().to_string())
                    .collect();
                println!("  {}", colored.join("  "));
                println!("{}", "─".repeat(50));
            }
        }
    }

    ctx.out.info("Ctrl+C to exit");

    let mut args = vec!["logs", "-f", "--tail=100"];
    if let Some(ref svc) = service {
        args.push(svc.as_str());
    }

    ctx.rt.compose_stream(&args)?;
    Ok(())
}
