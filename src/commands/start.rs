use anyhow::Result;

use crate::commands::ctx::Ctx;
use crate::commands::proxy;
use crate::hooks;

pub fn run(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    let mut ctx = Ctx::load(verbose, no_color)?;

    let args: Vec<&str> = match service.as_deref() {
        None => {
            hooks::apply_pre_start(&mut ctx.rt.project, &ctx.out, verbose, no_color)?;
            vec!["up", "-d"]
        }
        Some(svc) => vec!["up", "-d", svc],
    };

    let msg = match service.as_deref() {
        None => "Starting containers...".to_string(),
        Some(svc) => format!("Starting '{svc}'..."),
    };
    ctx.rt.compose_run(&args, &msg)?;

    let label = service.as_deref().unwrap_or("Containers");
    ctx.out.success(&format!("{label} started"));

    proxy::apply_sync(&ctx.rt.project, verbose, no_color);

    if service.is_none() {
        hooks::run_post_start(&ctx.rt.project, verbose, no_color);
    }

    Ok(())
}
