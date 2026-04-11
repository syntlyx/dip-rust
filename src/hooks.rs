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
/// Stderr from the hook is printed to the terminal so the user can see
/// progress/errors.  A non-zero exit code aborts the dip command.
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Result;

use crate::project::ProjectConfig;
use crate::utils::output::Output;

/// Run `.dip/hooks/pre-start` if it exists.
/// Returns env vars emitted by the hook (parsed from stdout).
pub fn run_pre_start(
    project: &ProjectConfig,
    verbose: bool,
    no_color: bool,
) -> Result<HashMap<String, String>> {
    let hook = project.dip_dir.join("hooks").join("pre-start");
    if !hook.exists() {
        return Ok(HashMap::new());
    }

    let out = Output::new(no_color);
    out.info(&format!("Running pre-start hook: {}", hook.display()));

    run_hook(&hook, project, verbose)
}

fn run_hook(
    path: &Path,
    project: &ProjectConfig,
    verbose: bool,
) -> Result<HashMap<String, String>> {
    if verbose {
        eprintln!("Hook: {}", path.display());
    }

    let output = Command::new(path)
        .envs(project.get_env())
        // Let stderr flow to the terminal so the user sees hook output
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| {
            anyhow::anyhow!(
                "Failed to run hook {}: {e}\n(Is it executable? Try: chmod +x {})",
                path.display(),
                path.display()
            )
        })?;

    if !output.status.success() {
        anyhow::bail!(
            "Hook '{}' exited with status {}",
            path.display(),
            output.status
        );
    }

    // Parse stdout as env vars
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_env_output(&stdout))
}

/// Parse lines of the form:
///   KEY=VALUE
///   export KEY=VALUE
///   export KEY="VALUE"
fn parse_env_output(output: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Strip leading `export ` keyword
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().to_string();
            // Strip surrounding quotes from value
            let value = value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            map.insert(key, value);
        }
    }
    map
}
