use anyhow::Result;

use crate::hooks;
use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::ensure_executable;
use crate::utils::output::Output;

pub fn run(script: &str, args: &[String], verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    let mut project = ProjectConfig::load()?;

    // Also run pre-start hook so scripts get AWS credentials etc.
    let hook_env = hooks::run_pre_start(&project, verbose, no_color)?;
    if !hook_env.is_empty() {
        project.merge_env(hook_env);
    }

    let commands_dir = project.dip_dir.join("commands");
    let script_path = commands_dir.join(script);

    if !script_path.exists() {
        // Show available scripts
        let available = list_scripts(&commands_dir);
        if available.is_empty() {
            anyhow::bail!(
                "Script '{}' not found. No scripts in {}",
                script,
                commands_dir.display()
            );
        } else {
            anyhow::bail!(
                "Script '{}' not found in {}.\nAvailable: {}",
                script,
                commands_dir.display(),
                available.join(", ")
            );
        }
    }

    ensure_executable(&script_path)?;

    if verbose {
        out.info(&format!(
            "Running: {} {}",
            script_path.display(),
            args.join(" ")
        ));
    }

    let status = std::process::Command::new(&script_path)
        .args(args)
        .envs(project.get_env())
        .status()?;

    if !status.success() {
        anyhow::bail!("Script '{}' exited with status {}", script, status);
    }

    Ok(())
}

fn list_scripts(commands_dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(commands_dir) else {
        return vec![];
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}
