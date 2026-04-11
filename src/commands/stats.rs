use anyhow::Result;

use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::output::Output;

pub fn run(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    let project = ProjectConfig::load()?;
    let project_name = project.project_name.clone();
    let rt = Runtime::new(project, verbose, no_color);

    out.info("Showing live resource usage (Ctrl+C to exit)...");

    let format = "table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.NetIO}}\t{{.BlockIO}}";

    if let Some(ref svc) = service {
        let raw = rt.compose_capture(&["ps", "-q", svc])?;
        let ids: Vec<String> = raw
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if ids.is_empty() {
            out.error(&format!("No running container found for service '{svc}'"));
            return Ok(());
        }
        let mut args = vec!["stats", "--format", format];
        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        args.extend_from_slice(&id_refs);
        // raw_stream passes through stdin/stdout/stderr so Ctrl+C works correctly
        rt.raw_stream(&args)?;
        return Ok(());
    }

    // No service: stats for all project containers
    let filter = format!("label=com.docker.compose.project={project_name}");
    let ids_raw = rt.raw_capture(&["ps", "-q", "--filter", &filter])?;
    let ids: Vec<&str> = ids_raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    if ids.is_empty() {
        out.info("No running containers for this project");
        return Ok(());
    }

    let mut args = vec!["stats", "--format", format];
    args.extend_from_slice(&ids);
    let _ = rt.raw_stream(&args);

    Ok(())
}
