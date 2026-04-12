use anyhow::Result;

use crate::commands::ctx::Ctx;
use crate::utils::notify;

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;
    let project = ctx.rt.project.project_name.clone();

    let result = ctx.rt.compose_run(&["pull"], "Pulling latest images...");
    if result.is_ok() {
        ctx.out.success("Images pulled");
    }
    notify::notify_result(result, &project, "pull")
}
