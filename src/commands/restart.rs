use anyhow::Result;

use crate::commands::ctx::Ctx;
use crate::commands::proxy;
use crate::hooks;

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let mut ctx = Ctx::load(verbose, no_color)?;

    hooks::run_pre_stop(&ctx.rt.project, verbose, no_color);

    ctx.rt.compose_run(&["stop"], "Stopping containers...")?;
    ctx.out.info("Stopped");

    hooks::run_post_stop(&ctx.rt.project, verbose, no_color);

    hooks::apply_pre_start(&mut ctx.rt.project, &ctx.out, verbose, no_color)?;

    ctx.rt.compose_run(
        &["up", "-d"],
        "Starting containers with fresh environment...",
    )?;
    ctx.out.success("Containers restarted");

    proxy::apply_sync(&ctx.rt.project, verbose, no_color);
    hooks::run_post_start(&ctx.rt.project, verbose, no_color);

    Ok(())
}
