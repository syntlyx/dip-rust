use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::Result;

use crate::templates;
use crate::utils::output::Output;

pub fn run(template: Option<&str>, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let cwd = std::env::current_dir()?;
    let dip_dir = cwd.join(".dip");

    if dip_dir.exists() {
        anyhow::bail!(".dip/ already exists in this directory — already a dip project");
    }

    let tmpl = match template {
        Some(name) => templates::find(name)?,
        None => templates::default(),
    };

    println!("Initializing new dip project in {}", cwd.display());
    if tmpl.name != "default" {
        println!("Template: {} — {}", tmpl.name, tmpl.description);
    }
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
    let env_content = {
        let mut s = format!("PROJECT_NAME={project_name}\nDOMAIN={domain}\n");
        let extra = tmpl.extra_env.replace("{name}", &project_name);
        s.push_str(&extra);
        s
    };
    write_file(&dip_dir.join("default.env"), &env_content)?;
    write_file(&dip_dir.join(".env"), &env_content)?;

    // ── docker-compose.yml ────────────────────────────────────────────────────
    write_file(&dip_dir.join("docker-compose.yml"), tmpl.compose)?;

    // ── Dockerfile ────────────────────────────────────────────────────────────
    if let Some(dockerfile) = tmpl.dockerfile {
        let path = cwd.join("Dockerfile");
        if path.exists() {
            out.warning("Dockerfile already exists — skipping");
        } else {
            write_file(&path, dockerfile)?;
        }
    }

    // ── hooks ─────────────────────────────────────────────────────────────────
    write_executable(&dip_dir.join("hooks/pre-start"), templates::PRE_START)?;

    // ── commands ──────────────────────────────────────────────────────────────
    write_file(
        &dip_dir.join("commands/utils/color.sh"),
        templates::COLOR_SH,
    )?;
    let hello = templates::HELLO.replace("{name}", &project_name);
    write_executable(&dip_dir.join("commands/hello"), &hello)?;

    // ── .gitignore ────────────────────────────────────────────────────────────
    update_gitignore(&cwd)?;

    // ── summary ───────────────────────────────────────────────────────────────
    println!();
    out.success(&format!("Project '{project_name}' initialized"));
    println!();
    if tmpl.dockerfile.is_some() {
        println!("  Dockerfile");
    }
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
        return Ok(());
    }

    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;

    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(f)?;
    }
    writeln!(f, "{entry}")?;
    Ok(())
}
