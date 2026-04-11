use std::path::Path;

use anyhow::Result;
use colored::Colorize;

use crate::db;
use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::output::Output;

pub fn run_dump(
    output_path: &Path,
    service: Option<&str>,
    verbose: bool,
    no_color: bool,
) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    let project = ProjectConfig::load()?;

    // Try label-based detection first
    let labeled = db::detect_by_labels(&project, verbose)?;

    if !labeled.is_empty() {
        let svc = resolve_service(labeled, service, "dump")?;
        out.info(&format!(
            "Detected backend: {} (service: {})",
            svc.backend.name(),
            svc.service_name
        ));
        return svc
            .backend
            .dump(&svc.container_id, &svc.config, output_path, &out);
    }

    // Fallback: legacy mode — service named "db", creds from .env
    out.info("No dip.db labels found — using legacy mode (service: db)");
    let env = project.get_env();
    let (backend, config) = db::detect(&env)?;
    let rt = Runtime::new(project, verbose, no_color);
    let container_id = rt.get_container_id("db")?;

    out.info(&format!("Detected backend: {}", backend.name()));
    backend.dump(&container_id, &config, output_path, &out)
}

pub fn run_import(
    input_path: &Path,
    service: Option<&str>,
    verbose: bool,
    no_color: bool,
) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    if !input_path.exists() {
        anyhow::bail!("File not found: {}", input_path.display());
    }

    let project = ProjectConfig::load()?;

    // Try label-based detection first
    let labeled = db::detect_by_labels(&project, verbose)?;

    if !labeled.is_empty() {
        let svc = resolve_service(labeled, service, "import")?;
        out.info(&format!(
            "Detected backend: {} (service: {})",
            svc.backend.name(),
            svc.service_name
        ));
        return svc
            .backend
            .import(&svc.container_id, &svc.config, input_path, &out);
    }

    // Fallback: legacy mode — service named "db", creds from .env
    out.info("No dip.db labels found — using legacy mode (service: db)");
    let env = project.get_env();
    let (backend, config) = db::detect(&env)?;
    let rt = Runtime::new(project, verbose, no_color);
    let container_id = rt.get_container_id("db")?;

    out.info(&format!("Detected backend: {}", backend.name()));
    backend.import(&container_id, &config, input_path, &out)
}

pub fn run_list(verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;
    let project = ProjectConfig::load()?;
    let services = db::detect_by_labels(&project, verbose)?;

    out.section("db services", || {
        if services.is_empty() {
            println!(
                "  {}",
                "No dip.db labels found — checking legacy mode…".dimmed()
            );
            let env = project.get_env();
            match db::detect(&env) {
                Ok((backend, cfg)) => println!(
                    "  {:20} {:10} db: {}  user: {}",
                    "db (legacy)".cyan(),
                    backend.name().yellow(),
                    cfg.db_name.dimmed(),
                    cfg.user.dimmed(),
                ),
                Err(_) => println!("  {}", "No database configuration found".yellow()),
            }
        } else {
            for svc in &services {
                println!(
                    "  {:20} {:10} db: {}  user: {}",
                    svc.service_name.cyan(),
                    svc.backend.name().yellow(),
                    svc.config.db_name.dimmed(),
                    svc.config.user.dimmed(),
                );
            }
        }
    });
    Ok(())
}

/// Pick the right DbService from the list, honoring `--service` when there are multiple.
fn resolve_service(
    mut services: Vec<db::DbService>,
    service: Option<&str>,
    op: &str,
) -> Result<db::DbService> {
    if services.len() == 1 {
        return Ok(services.remove(0));
    }

    // Multiple DB services → --service is required
    match service {
        Some(name) => {
            let pos = services
                .iter()
                .position(|s| s.service_name == name)
                .ok_or_else(|| {
                    let names: Vec<&str> =
                        services.iter().map(|s| s.service_name.as_str()).collect();
                    anyhow::anyhow!(
                        "Service '{}' not found. Available DB services: {}",
                        name,
                        names.join(", ")
                    )
                })?;
            Ok(services.remove(pos))
        }
        None => {
            let names: Vec<&str> = services.iter().map(|s| s.service_name.as_str()).collect();
            anyhow::bail!(
                "Multiple DB services found: {}\n  \
                 Use --service to specify which one, e.g.:\n  \
                 dip db {} <file> --service {}",
                names.join(", "),
                op,
                names[0],
            )
        }
    }
}
