use anyhow::Result;

use crate::commands::proxy;
use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::output::Output;

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;
    let project = ProjectConfig::load()?;
    let rt = Runtime::new(project.clone(), verbose, no_color);

    rt.compose_run(&["stop"], "Stopping containers...")?;
    out.info("Stopped");

    rt.compose_run(
        &["up", "-d"],
        "Starting containers with fresh environment...",
    )?;
    out.success("Containers restarted");

    proxy::apply_sync(&project, verbose, no_color);

    Ok(())
}
