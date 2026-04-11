use anyhow::Result;

use crate::commands::ctx::Ctx;
use crate::commands::proxy;
use crate::hooks;

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;

    hooks::run_pre_stop(&ctx.rt.project, verbose, no_color);

    ctx.rt.compose_run(&["stop"], "Stopping containers...")?;
    ctx.out.success("Containers stopped");

    proxy::apply_unsync(&ctx.rt.project, verbose, no_color);
    hooks::run_post_stop(&ctx.rt.project, verbose, no_color);

    Ok(())
}
