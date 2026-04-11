use anyhow::Result;

use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::output::Output;

pub fn run_shell(service: &str, shell_type: &str, verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    let project = ProjectConfig::load()?;
    let rt = Runtime::new(project, verbose, no_color);

    out.info(&format!("Opening {shell_type} shell in '{service}'..."));

    // Try requested shell, fall back to sh
    let result = rt.compose_stream(&["exec", "-it", service, shell_type]);
    if result.is_err() && shell_type != "sh" {
        out.warning(&format!("{shell_type} not found, falling back to sh"));
        rt.compose_stream(&["exec", "-it", service, "sh"])?;
    } else {
        result?;
    }
    Ok(())
}

pub fn run_exec(service: &str, command: &[String], verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    let project = ProjectConfig::load()?;
    let rt = Runtime::new(project, verbose, no_color);

    if command.is_empty() {
        anyhow::bail!("Please provide a command to execute");
    }

    // Join args and pass to sh -c so shell syntax, pipes, and variable expansion work.
    // -i keeps stdin open; we omit -t because exec is often called from scripts without a TTY.
    let full_cmd = command.join(" ");
    let full_cmd = full_cmd.trim();

    out.info(&format!("Running '{full_cmd}' in '{service}'..."));
    rt.compose_stream(&["exec", "-i", service, "sh", "-c", full_cmd])?;
    Ok(())
}
