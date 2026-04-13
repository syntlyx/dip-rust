use anyhow::Result;

use crate::commands::ctx::Ctx;

pub fn run(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;

    if let Some(ref svc) = service {
        ctx.rt.compose_stream(&["top", svc], false)?;
    } else {
        ctx.rt.compose_stream(&["top"], false)?;
    }
    Ok(())
}
