use anyhow::Result;

use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::output::Output;

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;
    let project = ProjectConfig::load()?;
    let rt = Runtime::new(project, verbose, no_color);

    rt.compose_run(&["rm", "-f", "--stop"], "Removing stopped containers...")?;
    rt.raw_stream(&["image", "prune", "-f"])?;
    out.success("Cleanup complete");
    Ok(())
}
