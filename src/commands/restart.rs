use anyhow::Result;

use crate::commands::ctx::Ctx;
use crate::commands::proxy;
use crate::hooks;

pub fn run(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    let mut ctx = Ctx::load(verbose, no_color)?;

    let svc = service.as_deref();

    // Stop
    let stop_args: Vec<&str> = match svc {
        None => {
            hooks::run_pre_stop(&ctx.rt.project, verbose, no_color);
            vec!["stop"]
        }
        Some(s) => vec!["stop", s],
    };
    let stop_msg = match svc {
        None => "Stopping containers...".to_string(),
        Some(s) => format!("Stopping '{s}'..."),
    };
    ctx.rt.compose_run(&stop_args, &stop_msg)?;
    ctx.out.info("Stopped");

    if svc.is_none() {
        hooks::run_post_stop(&ctx.rt.project, verbose, no_color);
        hooks::apply_pre_start(&mut ctx.rt.project, &ctx.out, verbose, no_color)?;
    }

    // Start
    let up_args: Vec<&str> = match svc {
        None => vec!["up", "-d"],
        Some(s) => vec!["up", "-d", s],
    };
    let up_msg = match svc {
        None => "Starting containers with fresh environment...".to_string(),
        Some(s) => format!("Starting '{s}'..."),
    };
    ctx.rt.compose_run(&up_args, &up_msg)?;

    let label = svc.unwrap_or("Containers");
    ctx.out.success(&format!("{label} restarted"));

    proxy::apply_sync(&ctx.rt.project, verbose, no_color);

    if svc.is_none() {
        hooks::run_post_start(&ctx.rt.project, verbose, no_color);
    }

    Ok(())
}
