use anyhow::Result;
use colored::Colorize;

use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::output::Output;

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    // Docker version
    let version_raw = std::process::Command::new("docker")
        .args([
            "version",
            "--format",
            "Client: {{.Client.Version}}, Server: {{.Server.Version}}",
        ])
        .output()?;
    let docker_version = String::from_utf8_lossy(&version_raw.stdout)
        .trim()
        .to_string();

    // Docker info
    let info_raw = std::process::Command::new("docker")
        .args(["info", "--format",
            "{{.Containers}}\n{{.ContainersRunning}}\n{{.ContainersPaused}}\n{{.ContainersStopped}}\n{{.Images}}"])
        .output()?;
    let info_str = String::from_utf8_lossy(&info_raw.stdout).into_owned();
    let info: Vec<&str> = info_str.lines().collect();

    let dip_version = env!("CARGO_PKG_VERSION");

    println!("{}", "─".repeat(60));
    println!("  {}", "System Overview".bold().blue());
    println!("{}", "─".repeat(60));
    println!("  {:20} {}", "dip version:".cyan(), dip_version);
    println!("  {:20} {}", "Docker:".cyan(), docker_version);
    if info.len() >= 5 {
        println!("  {:20} {}", "Images:".cyan(), info[4]);
        println!("  {:20}", "Containers:".cyan());
        println!("    {:18} {}", "Total:", info[0]);
        println!("    {:18} {}", "Running:", info[1].green());
        println!("    {:18} {}", "Paused:", info[2].yellow());
        println!("    {:18} {}", "Stopped:", info[3].red());
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
