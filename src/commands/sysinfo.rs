use crate::utils::style::Stylize;
use anyhow::Result;

use crate::project::ProjectConfig;
use crate::runtime::{self, Runtime};
use crate::utils::output::Output;

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let backend = runtime::active_backend();
    let runtime_name = backend.name();
    backend.check_daemon()?;
    let backend_info = backend.system_info().ok();

    let dip_version = env!("CARGO_PKG_VERSION");

    println!("{}", "─".repeat(60));
    println!("  {}", "System Overview".bold().blue());
    println!("{}", "─".repeat(60));
    println!("  {:20} {}", "dip version:".cyan(), dip_version);
    println!("  {:20} {}", "Runtime:".cyan(), runtime_name);

    if let Some(info) = backend_info {
        if !info.version.is_empty() {
            println!("  {:20} {}", info.version_label.cyan(), info.version);
        }
        if let Some(images) = info.images {
            println!("  {:20} {}", "Images:".cyan(), images);
        }
        if let Some(containers) = info.containers {
            println!("  {:20}", "Containers:".cyan());
            println!("    {:18} {}", "Total:", containers.total);
            println!("    {:18} {}", "Running:", containers.running.green());
            println!("    {:18} {}", "Paused:", containers.paused.yellow());
            println!("    {:18} {}", "Stopped:", containers.stopped.red());
        }
    }

    // Project info if we're inside a project
    match ProjectConfig::load() {
        Ok(project) => {
            println!("{}", "─".repeat(60));
            println!(
                "  {} {}",
                "Project:".cyan(),
                project.project_name.bold().blue()
            );
            println!("  {:20} {}", "Root:", project.root_dir.display());
            println!(
                "  {:20} {}",
                "Compose file:",
                project.compose_file.display()
            );

            // List services
            let rt = Runtime::new(project, verbose, no_color);
            if let Ok(services_raw) = rt.compose_capture(&["config", "--services"]) {
                let services: Vec<&str> = services_raw
                    .lines()
                    .map(|l| l.trim())
                    .filter(|l| !l.is_empty())
                    .collect();
                if !services.is_empty() {
                    println!("  {:20}", "Services:".cyan());
                    for s in services {
                        println!("    {} {}", "◦".green(), s);
                    }
                }
            }
        }
        Err(_) => {
            println!("{}", "─".repeat(60));
            out.info("Not inside a dip project (no .dip directory found)");
        }
    }

    println!("{}", "─".repeat(60));
    Ok(())
}
