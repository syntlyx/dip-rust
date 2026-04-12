/// Hook runner for `.dip/hooks/<hook-name>` scripts.
///
/// Hooks are plain executable files.  Their **stdout** is parsed as
/// environment variables (`KEY=VALUE` or `export KEY=VALUE`), which are then
/// merged into the project environment before docker-compose is invoked.
/// This is the canonical way to inject dynamic credentials such as:
///
///   #!/usr/bin/env bash
///   aws configure export-credentials --format env
///
/// stdout visibility:
///   pre-start  — silent by default (may contain secrets); shown only with -v
///   all others — always streamed to the terminal (health checks, messages)
///
/// Stderr is always forwarded to the terminal.
/// A non-zero exit code aborts the dip command.
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Result;

use crate::project::ProjectConfig;
use crate::utils::ensure_executable;
use crate::utils::env;
use crate::utils::output::Output;

/// Run `.dip/hooks/pre-start` if it exists.
/// Returns env vars emitted by the hook (parsed from stdout).
pub fn run_pre_start(
    project: &ProjectConfig,
    verbose: bool,
    no_color: bool,
) -> Result<HashMap<String, String>> {
    // pre-start stdout is silent by default — it may contain secrets (AWS keys etc.)
    // Pass show_stdout=verbose so `-v` reveals what was exported.
    run_named_hook("pre-start", project, verbose, no_color, verbose)
}

/// Run the `pre-start` hook and merge any exported env vars into the project.
pub fn apply_pre_start(
    project: &mut ProjectConfig,
    out: &Output,
    verbose: bool,
    no_color: bool,
) -> Result<()> {
    let hook_env = run_pre_start(project, verbose, no_color)?;
    if !hook_env.is_empty() {
        out.info(&format!("Hook exported {} variable(s)", hook_env.len()));
        project.merge_env(hook_env);
    }
    Ok(())
}

/// Run `.dip/hooks/post-start` if it exists (best-effort, errors are warnings).
pub fn run_post_start(project: &ProjectConfig, verbose: bool, no_color: bool) {
    if let Err(e) = run_named_hook("post-start", project, verbose, no_color, true) {
        Output::new(no_color).warning(&format!("post-start hook: {e}"));
    }
}

/// Run `.dip/hooks/pre-stop` if it exists (best-effort, errors are warnings).
pub fn run_pre_stop(project: &ProjectConfig, verbose: bool, no_color: bool) {
    if let Err(e) = run_named_hook("pre-stop", project, verbose, no_color, true) {
        Output::new(no_color).warning(&format!("pre-stop hook: {e}"));
    }
}

/// Run `.dip/hooks/post-stop` if it exists (best-effort, errors are warnings).
pub fn run_post_stop(project: &ProjectConfig, verbose: bool, no_color: bool) {
    if let Err(e) = run_named_hook("post-stop", project, verbose, no_color, true) {
        Output::new(no_color).warning(&format!("post-stop hook: {e}"));
    }
}

fn run_named_hook(
    name: &str,
    project: &ProjectConfig,
    verbose: bool,
    no_color: bool,
    show_stdout: bool,
) -> Result<HashMap<String, String>> {
    let hook = project.dip_dir.join("hooks").join(name);
    if !hook.exists() {
        return Ok(HashMap::new());
    }
    let out = Output::new(no_color);
    out.info(&format!("Running {name} hook..."));
    run_hook(&hook, project, verbose, show_stdout)
}

fn run_hook(
    path: &Path,
    project: &ProjectConfig,
    verbose: bool,
    show_stdout: bool,
) -> Result<HashMap<String, String>> {
    crate::utils::log_verbose(verbose, &format!("Hook: {}", path.display()));

    ensure_executable(path)?;

    let mut child = Command::new(path)
        .envs(project.get_env())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to run hook {}: {e}", path.display()))?;

    let mut captured = String::new();
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap_or_default();
            if show_stdout {
                println!("  {line}");
            }
            captured.push_str(&line);
            captured.push('\n');
        }
    }

    let status = child
        .wait()
        .map_err(|e| anyhow::anyhow!("Hook wait failed: {e}"))?;

    if !status.success() {
        anyhow::bail!("Hook '{}' exited with status {}", path.display(), status);
    }

    Ok(env::parse_env_str(&captured))
}
