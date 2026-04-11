use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::Result;

use crate::utils::output::Output;

pub fn run(no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let cwd = std::env::current_dir()?;
    let dip_dir = cwd.join(".dip");

    if dip_dir.exists() {
        anyhow::bail!(".dip/ already exists in this directory — already a dip project");
    }

    println!("Initializing new dip project in {}", cwd.display());
    println!();

    let default_name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-project")
        .to_string();

    let project_name = prompt("Project name", &default_name)?;
    let domain = prompt("Domain", &format!("{project_name}.test"))?;

    // ── Create directories ────────────────────────────────────────────────────
    fs::create_dir_all(dip_dir.join("hooks"))?;
    fs::create_dir_all(dip_dir.join("commands/utils"))?;

    // ── .env / default.env ────────────────────────────────────────────────────
    let env_content = env_template(&project_name, &domain);
    write_file(&dip_dir.join("default.env"), &env_content)?;
    write_file(&dip_dir.join(".env"), &env_content)?;

    // ── docker-compose.yml ────────────────────────────────────────────────────
    write_file(&dip_dir.join("docker-compose.yml"), DOCKER_COMPOSE)?;

    // ── hooks ─────────────────────────────────────────────────────────────────
    write_executable(&dip_dir.join("hooks/pre-start"), PRE_START_HOOK)?;

    // ── commands ──────────────────────────────────────────────────────────────
    write_file(&dip_dir.join("commands/utils/color.sh"), COLOR_SH)?;
    write_executable(
        &dip_dir.join("commands/hello"),
        &hello_command(&project_name),
    )?;

    // ── .gitignore ────────────────────────────────────────────────────────────
    update_gitignore(&cwd)?;

    // ── summary ───────────────────────────────────────────────────────────────
    println!();
    out.success(&format!("Project '{project_name}' initialized"));
    println!();
    println!("  .dip/");
    println!("  ├── default.env          ← commit this");
    println!("  ├── .env                 ← gitignored, local overrides");
    println!("  ├── docker-compose.yml");
    println!("  ├── hooks/");
    println!("  │   └── pre-start        ← runs before dip start");
    println!("  └── commands/");
    println!("      ├── utils/color.sh");
    println!("      └── hello            ← dip run hello");
    println!();
    out.info("Edit .dip/docker-compose.yml, then run: dip start");

    Ok(())
}

// ─── templates ────────────────────────────────────────────────────────────────

fn env_template(project_name: &str, domain: &str) -> String {
    format!(
        "PROJECT_NAME={project_name}\n\
         DOMAIN={domain}\n"
    )
}

const DOCKER_COMPOSE: &str = "\
services:
  app:
    image: nginx:alpine
    volumes:
      - ${PROJECT_ROOT}:/usr/share/nginx/html:ro
    env_file:
      - ${ENV_FILE}
    labels:
      dip.host: \"${DOMAIN}:80\"
";

const PRE_START_HOOK: &str = "\
#!/usr/bin/env bash
# Pre-start hook — runs before containers start.
# Stdout is parsed as KEY=VALUE env vars and injected into docker-compose.
#
# Example: export AWS credentials
# aws configure export-credentials --format env
";

const COLOR_SH: &str = "\
# ANSI color helpers — source this in any dip command:
#   source \"${DIP_DIR}/commands/utils/color.sh\"

RED='\\033[0;31m'
GREEN='\\033[0;32m'
YELLOW='\\033[1;33m'
BLUE='\\033[0;34m'
CYAN='\\033[0;36m'
BOLD='\\033[1m'
NOFORMAT='\\033[0m'

msg() {
  echo -e \"$*\"
}
";

fn hello_command(project_name: &str) -> String {
    format!(
        "\
#!/usr/bin/env bash
# Description: Example dip command — run with: dip run hello

set -e

source \"${{DIP_DIR}}/commands/utils/color.sh\"

msg \"${{GREEN}}[INFO]${{NOFORMAT}} Hello from {project_name}!\"
msg \"${{YELLOW}}[INFO]${{NOFORMAT}} Args: $*\"
"
    )
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn prompt(label: &str, default: &str) -> Result<String> {
    print!("  {label} [{default}]: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed)
    }
}

fn write_file(path: &Path, content: &str) -> Result<()> {
    fs::write(path, content).map_err(|e| anyhow::anyhow!("Failed to write {}: {e}", path.display()))
}

fn write_executable(path: &Path, content: &str) -> Result<()> {
    write_file(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn update_gitignore(root: &Path) -> Result<()> {
    let path = root.join(".gitignore");
    let entry = ".dip/.env";

    let existing = fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == entry) {
        return Ok(()); // already there
    }

    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;

    // Add a newline separator if file doesn't end with one
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(f)?;
    }
    writeln!(f, "{entry}")?;
    Ok(())
}
