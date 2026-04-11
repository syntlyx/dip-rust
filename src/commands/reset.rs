use anyhow::Result;

use crate::commands::ctx::Ctx;

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;

    ctx.rt.compose_run(&["stop"], "Stopping containers...")?;
    ctx.out.info("Stopped");
    ctx.rt
        .compose_run(&["rm", "-f"], "Removing containers...")?;
    ctx.out.info("Removed");
    ctx.rt
        .compose_run(&["up", "-d"], "Starting containers...")?;
    ctx.out.success("Reset complete — containers are running");
    Ok(())
}
