use anyhow::Result;

use crate::commands::ctx::Ctx;

pub fn run(verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;
    ctx.rt.compose_run(&["pull"], "Pulling latest images...")?;
    ctx.out.success("Images pulled");
    Ok(())
}
