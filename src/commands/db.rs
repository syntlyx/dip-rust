use std::path::Path;

use anyhow::Result;
use colored::Colorize;

use crate::commands::ctx::Ctx;
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
    let project_name = project.project_name.clone();

    // Try label-based detection first
    let labeled = db::detect_by_labels(&project, verbose)?;

    let result = if !labeled.is_empty() {
        let svc = resolve_service(labeled, service, "dump")?;
        out.info(&format!(
            "Detected backend: {} (service: {})",
            svc.backend.name(),
            svc.service_name
        ));
        svc.backend
            .dump(&svc.container_id, &svc.config, output_path, &out)
    } else {
        // Fallback: legacy mode — service named "db", creds from .env
        out.info("No dip.db labels found — using legacy mode (service: db)");
        let env = project.get_env();
        let (backend, config) = db::detect(&env)?;
        let rt = Runtime::new(project, verbose, no_color);
        let container_id = rt.get_container_id("db")?;
        out.info(&format!("Detected backend: {}", backend.name()));
        backend.dump(&container_id, &config, output_path, &out)
    };

    crate::utils::notify::notify_result(result, &project_name, "db dump")
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
    let project_name = project.project_name.clone();

    // Try label-based detection first
    let labeled = db::detect_by_labels(&project, verbose)?;

    let result = if !labeled.is_empty() {
        let svc = resolve_service(labeled, service, "import")?;
        out.info(&format!(
            "Detected backend: {} (service: {})",
            svc.backend.name(),
            svc.service_name
        ));
        svc.backend
            .import(&svc.container_id, &svc.config, input_path, &out)
    } else {
        // Fallback: legacy mode — service named "db", creds from .env
        out.info("No dip.db labels found — using legacy mode (service: db)");
        let env = project.get_env();
        let (backend, config) = db::detect(&env)?;
        let rt = Runtime::new(project, verbose, no_color);
        let container_id = rt.get_container_id("db")?;
        out.info(&format!("Detected backend: {}", backend.name()));
        backend.import(&container_id, &config, input_path, &out)
    };

    crate::utils::notify::notify_result(result, &project_name, "db import")
}

pub fn run_list(format: Option<&str>, verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;
    let project = ProjectConfig::load()?;
    let services = db::detect_by_labels(&project, verbose)?;

    if format == Some("json") {
        let env = project.get_env();

        // Collect labeled services; fall back to legacy detection if none found.
        let rows: Vec<serde_json::Value> = if services.is_empty() {
            match db::detect(&env) {
                Ok((backend, cfg)) => vec![serde_json::json!({
                    "service": "db",
                    "backend": backend.name(),
                    "database": cfg.db_name,
                    "user": cfg.user,
                    "legacy": true,
                })],
                Err(_) => vec![],
            }
        } else {
            services
                .iter()
                .map(|s| {
                    serde_json::json!({
                        "service": s.service_name,
                        "backend": s.backend.name(),
                        "database": s.config.db_name,
                        "user": s.config.user,
                        "legacy": false,
                    })
                })
                .collect()
        };

        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

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

pub fn run_console(service: Option<&str>, verbose: bool, no_color: bool) -> Result<i32> {
    let ctx = Ctx::load(verbose, no_color)?;
    Runtime::check_daemon()?;

    let labeled = db::detect_by_labels(&ctx.rt.project, verbose)?;

    let svc = if !labeled.is_empty() {
        resolve_service(labeled, service, "console")?
    } else {
        anyhow::bail!(
            "No dip.db labels found — add a `dip.db: postgres` or `dip.db: mysql` label to your DB service"
        );
    };

    ctx.out.info(&format!(
        "Opening {} console in '{}'...",
        svc.backend.name(),
        svc.service_name
    ));

    let cmd = svc.backend.console_cmd(&svc.config);
    let env_pairs = svc.backend.console_env(&svc.config);

    // Build: ["exec", "-e", "KEY=VAL", ..., "-it", service, cmd...]
    // Credentials go via -e so they don't appear in `ps aux` output.
    let mut args: Vec<String> = vec!["exec".into()];
    for (k, v) in &env_pairs {
        args.push("-e".into());
        args.push(format!("{k}={v}"));
    }
    args.push("-it".into());
    args.push(svc.service_name.clone());
    args.extend_from_slice(&cmd);

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    ctx.rt.compose_stream(&arg_refs, false)
}

pub fn run_migrate(
    from: &str,
    to: &str,
    tables: Option<&str>,
    verbose: bool,
    no_color: bool,
) -> Result<()> {
    Runtime::check_daemon()?;
    let project = ProjectConfig::load()?;
    let project_name = project.project_name.clone();
    let result = db::migrate::run_migrate(&project, from, to, tables, verbose, no_color);
    crate::utils::notify::notify_result(result, &project_name, &format!("migrate {from}→{to}"))
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
