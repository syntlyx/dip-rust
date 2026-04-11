use anyhow::Result;

use crate::commands::proxy;
use crate::hooks;
use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::output::Output;

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    let mut project = ProjectConfig::load()?;

    let hook_env = hooks::run_pre_start(&project, verbose, no_color)?;
    if !hook_env.is_empty() {
        out.info(&format!("Hook exported {} variable(s)", hook_env.len()));
        project.merge_env(hook_env);
    }

    let rt = Runtime::new(project.clone(), verbose, no_color);
    rt.compose_run(&["up", "-d"], "Starting containers...")?;
    out.success("Containers started");

    proxy::apply_sync(&project, verbose, no_color);
    hooks::run_post_start(&project, verbose, no_color);

    Ok(())
}
