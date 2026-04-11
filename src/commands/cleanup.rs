use anyhow::Result;

use crate::commands::ctx::Ctx;

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;

    ctx.rt
        .compose_run(&["rm", "-f", "--stop"], "Removing stopped containers...")?;
    ctx.rt.raw_stream(&["image", "prune", "-f"])?;
    ctx.out.success("Cleanup complete");
    Ok(())
}
