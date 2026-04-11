use anyhow::Result;

use crate::commands::ctx::Ctx;

pub fn run(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;
    let project_name = ctx.rt.project.project_name.clone();

    ctx.out
        .info("Showing live resource usage (Ctrl+C to exit)...");

    let format = "table {{.Name}}\t{{.CPUPerc}}\t{{.MemUsage}}\t{{.NetIO}}\t{{.BlockIO}}";

    if let Some(ref svc) = service {
        let raw = ctx.rt.compose_capture(&["ps", "-q", svc])?;
        let ids: Vec<String> = raw
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect();
        if ids.is_empty() {
            ctx.out
                .error(&format!("No running container found for service '{svc}'"));
            return Ok(());
        }
        let mut args = vec!["stats", "--format", format];
        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        args.extend_from_slice(&id_refs);
        // raw_stream passes through stdin/stdout/stderr so Ctrl+C works correctly
        ctx.rt.raw_stream(&args)?;
        return Ok(());
    }

    // No service: stats for all project containers
    let filter = format!("label=com.docker.compose.project={project_name}");
    let ids_raw = ctx.rt.raw_capture(&["ps", "-q", "--filter", &filter])?;
    let ids: Vec<&str> = ids_raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    if ids.is_empty() {
        ctx.out.info("No running containers for this project");
        return Ok(());
    }

    let mut args = vec!["stats", "--format", format];
    args.extend_from_slice(&ids);
    let _ = ctx.rt.raw_stream(&args);

    Ok(())
}
