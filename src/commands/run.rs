use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::commands::ctx::Ctx;
use crate::utils::ensure_executable;

pub fn run(script: Option<&str>, args: &[String], verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;
    let commands_dir = ctx.rt.project.dip_dir.join("commands");

    // No script given — print table and exit
    let Some(script) = script else {
        return print_commands_table(&commands_dir, &ctx.out);
    };

    let script_path = commands_dir.join(script);

    if !script_path.exists() {
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
                available
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    ensure_executable(&script_path)?;

    if verbose {
        ctx.out.info(&format!(
            "Running: {} {}",
            script_path.display(),
            args.join(" ")
        ));
    }

    let status = std::process::Command::new(&script_path)
        .args(args)
        .envs(ctx.rt.project.get_env())
        .status()?;

    if !status.success() {
        anyhow::bail!("Script '{}' exited with status {}", script, status);
    }

    Ok(())
}

// ─── table output ─────────────────────────────────────────────────────────────

fn print_commands_table(commands_dir: &Path, out: &crate::utils::output::Output) -> Result<()> {
    let scripts = list_scripts(commands_dir);

    if scripts.is_empty() {
        out.warning(&format!("No scripts found in {}", commands_dir.display()));
        return Ok(());
    }

    let name_width = scripts
        .iter()
        .map(|s| s.name.len())
        .max()
        .unwrap_or(0)
        .max(6);

    println!();
    println!("  {:<name_width$}  DESCRIPTION", "COMMAND");
    println!("  {}  {}", "─".repeat(name_width), "─".repeat(40));

    for s in &scripts {
        let desc = s.description.as_deref().unwrap_or("—");
        println!(
            "  {:<name_width$}  {}",
            s.name,
            desc,
            name_width = name_width
        );
    }

    println!();
    println!("  Run with: dip run <command> [args]");
    println!();

    Ok(())
}

// ─── script discovery ─────────────────────────────────────────────────────────

struct ScriptInfo {
    name: String,
    description: Option<String>,
}

/// Walk `commands_dir` recursively, collecting all executable-like files.
/// Subdirectories (e.g. `utils/`) are skipped — they're helpers, not commands.
fn list_scripts(commands_dir: &Path) -> Vec<ScriptInfo> {
    let Ok(entries) = fs::read_dir(commands_dir) else {
        return vec![];
    };

    let mut scripts: Vec<ScriptInfo> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|ft| ft.is_file()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            // Skip hidden files
            if name.starts_with('.') {
                return None;
            }
            let path = e.path();
            let description = read_description(&path);
            Some(ScriptInfo { name, description })
        })
        .collect();

    scripts.sort_by(|a, b| a.name.cmp(&b.name));
    scripts
}

/// Read the first `# Description:` comment from a script file.
fn read_description(path: &PathBuf) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let reader = BufReader::new(file);

    for line in reader.lines().take(10).filter_map(|l| l.ok()) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed
            .strip_prefix("# Description:")
            .or_else(|| trimmed.strip_prefix("#Description:"))
        {
            let desc = rest.trim().to_string();
            if !desc.is_empty() {
                return Some(desc);
            }
        }
    }
    None
}
