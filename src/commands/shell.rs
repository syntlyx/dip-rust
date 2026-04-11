use anyhow::Result;

use crate::commands::ctx::Ctx;

pub fn run_shell(service: &str, shell_type: &str, verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;

    ctx.out
        .info(&format!("Opening {shell_type} shell in '{service}'..."));

    // Try requested shell, fall back to sh
    let result = ctx.rt.compose_stream(&["exec", "-it", service, shell_type]);
    if result.is_err() && shell_type != "sh" {
        ctx.out
            .warning(&format!("{shell_type} not found, falling back to sh"));
        ctx.rt.compose_stream(&["exec", "-it", service, "sh"])?;
    } else {
        result?;
    }
    Ok(())
}

pub fn run_exec(service: &str, command: &[String], verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;

    if command.is_empty() {
        anyhow::bail!("Please provide a command to execute");
    }

    // Join args and pass to sh -c so shell syntax, pipes, and variable expansion work.
    // -i keeps stdin open; we omit -t because exec is often called from scripts without a TTY.
    let full_cmd = command.join(" ");
    let full_cmd = full_cmd.trim();

    ctx.out
        .info(&format!("Running '{full_cmd}' in '{service}'..."));
    ctx.rt
        .compose_stream(&["exec", "-i", service, "sh", "-c", full_cmd])?;
    Ok(())
}
