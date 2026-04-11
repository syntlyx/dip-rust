use anyhow::Result;

use crate::commands::ctx::Ctx;

pub fn run(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;

    let msg = match &service {
        Some(s) => format!("Building service '{s}'..."),
        None => "Building all services...".to_string(),
    };

    let mut args = vec!["build"];
    if let Some(ref s) = service {
        args.push(s.as_str());
    }
    ctx.rt.compose_run(&args, &msg)?;
    ctx.out.success("Build complete");

    Ok(())
}
