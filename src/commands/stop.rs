use anyhow::Result;

use crate::commands::ctx::Ctx;
use crate::commands::proxy;
use crate::hooks;

pub fn run(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;

    let args: Vec<&str> = match service.as_deref() {
        None => {
            hooks::run_pre_stop(&ctx.rt.project, verbose, no_color);
            vec!["stop"]
        }
        Some(svc) => vec!["stop", svc],
    };

    let msg = match service.as_deref() {
        None => "Stopping containers...".to_string(),
        Some(svc) => format!("Stopping '{svc}'..."),
    };
    ctx.rt.compose_run(&args, &msg)?;

    let label = service.as_deref().unwrap_or("Containers");
    ctx.out.success(&format!("{label} stopped"));

    proxy::apply_unsync(&ctx.rt.project, verbose, no_color);

    if service.is_none() {
        hooks::run_post_stop(&ctx.rt.project, verbose, no_color);
    }

    Ok(())
}
