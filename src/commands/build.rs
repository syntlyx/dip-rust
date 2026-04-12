use anyhow::Result;

use crate::commands::ctx::Ctx;
use crate::utils::notify;

pub fn run(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;
    let project = ctx.rt.project.project_name.clone();

    let msg = match &service {
        Some(s) => format!("Building service '{s}'..."),
        None => "Building all services...".to_string(),
    };

    let mut args = vec!["build"];
    if let Some(ref s) = service {
        args.push(s.as_str());
    }

    let result = ctx.rt.compose_run(&args, &msg);
    if result.is_ok() {
        ctx.out.success("Build complete");
    }
    notify::notify_result(result, &project, "build")
}
