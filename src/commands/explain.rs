use anyhow::Result;
use colored::Colorize;

use crate::commands::compose_config::{self, BuildConfig};
use crate::commands::ctx::Ctx;

pub fn run_build(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;
    let config = compose_config::load(&ctx.rt)?;

    let mut services: Vec<(&str, &BuildConfig)> = Vec::new();
    if let Some(name) = service.as_deref() {
        let svc = config
            .services
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("Service '{name}' not found"))?;
        let build = svc
            .build
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Service '{name}' has no build configuration"))?;
        services.push((name, build));
    } else {
        for (name, svc) in &config.services {
            if let Some(build) = &svc.build {
                services.push((name, build));
            }
        }
    }

    ctx.out.section("build config", || {
        if services.is_empty() {
            println!("  {}", "No services with build configuration".dimmed());
            return;
        }

        for (index, (name, build)) in services.iter().enumerate() {
            if index > 0 {
                println!();
            }
            print_build(name, build);
        }
    });

    Ok(())
}

fn print_build(name: &str, build: &BuildConfig) {
    println!("  {}", name.bold().green());

    match build.context_path() {
        Some(context) => {
            let status = if context.is_dir() {
                "exists".green()
            } else {
                "missing".red()
            };
            println!(
                "    {:12} {}  {}",
                "context:".dimmed(),
                context.display(),
                status
            );
        }
        None => println!("    {:12} {}", "context:".dimmed(), "missing".red()),
    }

    match build.dockerfile_path() {
        Some(path) => {
            let status = if path.is_file() {
                "exists".green()
            } else {
                "missing".red()
            };
            println!(
                "    {:12} {}  {}",
                "dockerfile:".dimmed(),
                path.display(),
                status
            );
        }
        None => println!("    {:12} {}", "dockerfile:".dimmed(), "unknown".red()),
    }

    match &build.target {
        Some(target) => {
            let status = match target_exists(build, target) {
                Some(true) => "exists".green().to_string(),
                Some(false) => "missing".red().to_string(),
                None => "unknown".yellow().to_string(),
            };
            println!("    {:12} {}  {}", "target:".dimmed(), target, status);
        }
        None => println!("    {:12} {}", "target:".dimmed(), "default stage".dimmed()),
    }

    if !build.args.is_empty() {
        let mut keys: Vec<&str> = build.args.keys().map(String::as_str).collect();
        keys.sort_unstable();
        println!("    {:12} {}", "args:".dimmed(), keys.join(", ").dimmed());
    }
}

fn target_exists(build: &BuildConfig, target: &str) -> Option<bool> {
    let path = build.dockerfile_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    Some(compose_config::dockerfile_stages(&content).contains(target))
}
