use std::fs;
use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::Result;
use include_dir::Dir;

use crate::templates;
use crate::utils::output::Output;

pub fn run(template: Option<&str>, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let cwd = std::env::current_dir()?;
    let dip_dir = cwd.join(".dip");

    if dip_dir.exists() {
        anyhow::bail!(".dip/ already exists in this directory — already a dip project");
    }

    // If no --template flag, show the interactive picker
    let tmpl = match template {
        Some(name) => Some(templates::find(name)?),
        None => prompt_template()?,
    };

    println!("Initializing new dip project in {}", cwd.display());
    if let Some(t) = &tmpl {
        println!("Template: {} — {}", t.name, t.description);
    }
    println!();

    let default_name = cwd
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("my-project")
        .to_string();

    let project_name = prompt("Project name", &default_name)?;
    let domain = prompt("Domain", &format!("{project_name}.test"))?;

    apply(&dip_dir, tmpl.map(|t| t.dir), &project_name, &domain)?;

    // Record the template so `dip update-commands` can skip detection later
    if let Some(t) = tmpl {
        fs::write(
            dip_dir.join(crate::commands::update_commands::TEMPLATE_MARKER),
            t.name,
        )?;
    }

    // ── summary ───────────────────────────────────────────────────────────────
    println!();
    out.success(&format!("Project '{project_name}' initialized"));
    println!();
    print_tree(&cwd, &dip_dir);
    println!();
    out.info("Edit .dip/docker-compose.yml, then run: dip start");

    Ok(())
}

/// Apply shared base + optional template into `root`, creating all files.
/// Separated from `run` so it can be called in tests without stdin prompts.
pub fn apply(
    root: &Path,
    template_dir: Option<&'static Dir<'static>>,
    project_name: &str,
    domain: &str,
) -> Result<()> {
    let vars: &[(&str, &str)] = &[("{PROJECT_NAME}", project_name), ("{DOMAIN}", domain)];

    // 1. Always apply the shared base
    copy_dir(templates::shared(), root, vars)?;

    // 2. Layer the specific template on top (overwrites shared files where they overlap)
    if let Some(dir) = template_dir {
        copy_dir(dir, root, vars)?;
    }

    // 3. .env is a gitignored local copy of default.env — write it from the same content
    let default_env_path = root.join("default.env");
    let env_content = fs::read_to_string(&default_env_path)?;
    fs::write(root.join(".env"), &env_content)?;

    // 4. .gitignore
    update_gitignore(root)?;

    Ok(())
}

// ─── directory copy ───────────────────────────────────────────────────────────

/// Recursively copy all files from an embedded `Dir` into `target`,
/// performing `{KEY}` → value substitution on all UTF-8 text files.
/// Files whose content starts with `#!` are made executable.
fn copy_dir(dir: &Dir, target: &Path, vars: &[(&str, &str)]) -> Result<()> {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(sub) => copy_dir(sub, target, vars)?,
            include_dir::DirEntry::File(file) => {
                let dest = target.join(file.path());

                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent)?;
                }

                let raw = file.contents();
                // Only substitute {PROJECT_NAME}/{DOMAIN} in .env files.
                // Everything else (docker-compose.yml, entrypoints, hooks, scripts)
                // uses ${VAR} shell/compose syntax and gets the values at runtime
                // from the environment dip passes in.
                let is_env_file = file
                    .path()
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.ends_with(".env"))
                    .unwrap_or(false);
                let content = match std::str::from_utf8(raw) {
                    Ok(text) if is_env_file => substitute(text, vars).into_bytes(),
                    Ok(text) => text.as_bytes().to_vec(),
                    Err(_) => raw.to_vec(), // binary file
                };

                fs::write(&dest, &content)
                    .map_err(|e| anyhow::anyhow!("Failed to write {}: {e}", dest.display()))?;

                // Make executable if file starts with a shebang
                if content.starts_with(b"#!") {
                    make_executable(&dest)?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn substitute(text: &str, vars: &[(&str, &str)]) -> String {
    let mut out = text.to_string();
    for (key, value) in vars {
        out = out.replace(key, value);
    }
    out
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(path)?;
        let mut perms = meta.permissions();
        let mode = perms.mode();
        perms.set_mode(mode | 0o755);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Show a numbered template menu and return the chosen template (or None for bare).
fn prompt_template() -> Result<Option<&'static templates::TemplateMeta>> {
    use crate::utils::style::Stylize;

    println!("  Choose a template:");
    println!();

    // Group by language category, print with numbers
    let all = templates::TEMPLATES;
    for (i, t) in all.iter().enumerate() {
        println!(
            "    {:>2})  {:<12} {}",
            (i + 1).to_string().cyan(),
            t.name.bold(),
            t.description.dimmed()
        );
    }
    println!(
        "    {:>2})  {:<12} {}",
        "0".cyan(),
        "none".bold(),
        "shared base only".dimmed()
    );
    println!();

    let input = prompt("Template", "0")?;
    let trimmed = input.trim();

    // Accept a number or a name
    let choice = if let Ok(n) = trimmed.parse::<usize>() {
        if n == 0 {
            None
        } else {
            let t = all.get(n - 1).ok_or_else(|| {
                anyhow::anyhow!(
                    "Invalid choice: {n}. Enter a number between 0 and {}",
                    all.len()
                )
            })?;
            Some(t)
        }
    } else if trimmed.is_empty() || trimmed == "none" {
        None
    } else {
        Some(templates::find(trimmed)?)
    };

    Ok(choice)
}

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

pub(crate) fn update_gitignore(root: &Path) -> Result<()> {
    let path = root.join(".gitignore");
    let entry = ".env";

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

// ─── tree printer ────────────────────────────────────────────────────────────

/// Print the actual file tree rooted at `dip_dir`, prefixed with the project
/// path so the user sees where everything landed.
fn print_tree(project_root: &Path, dip_dir: &Path) {
    // Show project root as context
    println!(
        "  {}/",
        project_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(".")
    );

    // Collect and sort top-level entries of the project root (only those we created)
    // We show .dip/ as a tree, Dockerfile at project root if present
    let mut top: Vec<String> = vec![];

    // Files that templates may place at the project root (Dockerfile, .dockerignore)
    for name in &["Dockerfile", ".dockerignore"] {
        if project_root.join(name).exists() {
            top.push(name.to_string());
        }
    }
    top.push(".dip/".to_string());

    let top_last = top.len().saturating_sub(1);
    for (i, entry) in top.iter().enumerate() {
        let branch = if i == top_last {
            "└──"
        } else {
            "├──"
        };
        if entry == ".dip/" {
            let prefix = if i == top_last { "    " } else { "│   " };
            println!("  {branch} .dip/");
            print_dir_tree(dip_dir, &format!("  {prefix}"), true);
        } else {
            println!("  {branch} {entry}");
        }
    }
}

/// Recursively print a directory tree.
fn print_dir_tree(dir: &Path, prefix: &str, _is_last: bool) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut items: Vec<(String, bool)> = entries // (name, is_dir)
        .filter_map(|e| e.ok())
        .map(|e| {
            let is_dir = e.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
            let name = e.file_name().into_string().unwrap_or_default();
            (name, is_dir)
        })
        .collect();

    // Dirs first, then files — both sorted alphabetically
    items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));

    let last_idx = items.len().saturating_sub(1);
    for (i, (name, is_dir)) in items.iter().enumerate() {
        let is_last = i == last_idx;
        let branch = if is_last { "└──" } else { "├──" };
        let annotation = annotation(name);

        if *is_dir {
            println!("{prefix}{branch} {name}/");
            let child_prefix = format!("{}{}   ", prefix, if is_last { " " } else { "│" });
            print_dir_tree(&dir.join(name), &child_prefix, is_last);
        } else {
            println!("{prefix}{branch} {name}{annotation}");
        }
    }
}

/// Short inline hints for well-known files.
fn annotation(name: &str) -> &'static str {
    match name {
        "default.env" => "  ← commit this",
        ".env" => "  ← gitignored, local overrides",
        _ => "",
    }
}

// ─── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    // ── substitute ────────────────────────────────────────────────────────────

    #[test]
    fn substitute_replaces_single_var() {
        let result = substitute("Hello {NAME}!", &[("{NAME}", "world")]);
        assert_eq!(result, "Hello world!");
    }

    #[test]
    fn substitute_replaces_multiple_vars() {
        let result = substitute(
            "project={PROJECT_NAME} domain={DOMAIN}",
            &[("{PROJECT_NAME}", "myapp"), ("{DOMAIN}", "myapp.test")],
        );
        assert_eq!(result, "project=myapp domain=myapp.test");
    }

    #[test]
    fn substitute_no_placeholders_unchanged() {
        let text = "nothing to replace here";
        assert_eq!(substitute(text, &[("{X}", "y")]), text);
    }

    #[test]
    fn substitute_multiple_occurrences() {
        let result = substitute("{A} and {A}", &[("{A}", "foo")]);
        assert_eq!(result, "foo and foo");
    }

    // ── update_gitignore ──────────────────────────────────────────────────────

    #[test]
    fn gitignore_created_when_missing() {
        let dir = tempdir().unwrap();
        update_gitignore(dir.path()).unwrap();
        let content = fs::read_to_string(dir.path().join(".gitignore")).unwrap();
        assert!(content.contains(".env"));
    }

    #[test]
    fn gitignore_entry_appended_to_existing_file() {
        let dir = tempdir().unwrap();
        let gi = dir.path().join(".gitignore");
        fs::write(&gi, "node_modules\n").unwrap();
        update_gitignore(dir.path()).unwrap();
        let content = fs::read_to_string(&gi).unwrap();
        assert!(content.contains("node_modules"));
        assert!(content.contains(".env"));
    }

    #[test]
    fn gitignore_not_duplicated() {
        let dir = tempdir().unwrap();
        let gi = dir.path().join(".gitignore");
        fs::write(&gi, ".env\n").unwrap();
        update_gitignore(dir.path()).unwrap();
        let content = fs::read_to_string(&gi).unwrap();
        assert_eq!(content.matches(".env").count(), 1);
    }

    #[test]
    fn gitignore_newline_added_before_entry_when_file_has_no_trailing_newline() {
        let dir = tempdir().unwrap();
        let gi = dir.path().join(".gitignore");
        fs::write(&gi, "dist").unwrap(); // no trailing newline
        update_gitignore(dir.path()).unwrap();
        let content = fs::read_to_string(&gi).unwrap();
        // Entry must be on its own line
        assert!(content.lines().any(|l| l == ".env"));
    }

    // ── apply (shared-only) ───────────────────────────────────────────────────

    #[test]
    fn apply_bare_creates_expected_structure() {
        let dip = tempdir().unwrap().path().join(".dip");

        apply(&dip, None, "testapp", "testapp.test").unwrap();

        assert!(dip.join("default.env").exists(), "default.env missing");
        assert!(dip.join(".env").exists(), ".env missing");
        assert!(
            dip.join("docker-compose.yml").exists(),
            "docker-compose.yml missing"
        );
        assert!(dip.join("hooks/pre-start").exists(), "pre-start missing");
        assert!(dip.join("commands/hello").exists(), "hello missing");
        assert!(
            dip.join("commands/utils/color.sh").exists(),
            "color.sh missing"
        );
    }

    #[test]
    fn apply_substitutes_placeholders_in_default_env() {
        let dip = tempdir().unwrap().path().join(".dip");
        apply(&dip, None, "myapp", "myapp.test").unwrap();

        let content = fs::read_to_string(dip.join("default.env")).unwrap();
        assert!(content.contains("myapp"), "PROJECT_NAME not substituted");
        assert!(content.contains("myapp.test"), "DOMAIN not substituted");
        assert!(
            !content.contains("{PROJECT_NAME}"),
            "placeholder still present"
        );
        assert!(!content.contains("{DOMAIN}"), "placeholder still present");
    }

    #[test]
    fn apply_env_matches_default_env() {
        let dip = tempdir().unwrap().path().join(".dip");
        apply(&dip, None, "proj", "proj.test").unwrap();

        let default = fs::read_to_string(dip.join("default.env")).unwrap();
        let env = fs::read_to_string(dip.join(".env")).unwrap();
        assert_eq!(default, env);
    }

    #[test]
    fn apply_adds_gitignore_entry() {
        let dip = tempdir().unwrap().path().join(".dip");
        apply(&dip, None, "proj", "proj.test").unwrap();

        let content = fs::read_to_string(dip.join(".gitignore")).unwrap();
        assert!(content.contains(".env"));
    }

    #[test]
    #[cfg(unix)]
    fn apply_shebang_files_are_executable() {
        let dip = tempdir().unwrap().path().join(".dip");
        apply(&dip, None, "proj", "proj.test").unwrap();

        for path in [dip.join("hooks/pre-start"), dip.join("commands/hello")] {
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "{} is not executable", path.display());
        }
    }

    // ── apply (with template) ─────────────────────────────────────────────────

    #[test]
    fn apply_nestjs_template_adds_dockerfile() {
        let dip = tempdir().unwrap().path().join(".dip");
        let tmpl = crate::templates::find("nestjs").unwrap();
        apply(&dip, Some(tmpl.dir), "api", "api.test").unwrap();

        assert!(dip.join("Dockerfile").exists(), "Dockerfile missing");
    }

    #[test]
    fn apply_nestjs_overrides_compose() {
        let dip = tempdir().unwrap().path().join(".dip");
        let tmpl = crate::templates::find("nestjs").unwrap();
        apply(&dip, Some(tmpl.dir), "api", "api.test").unwrap();

        let compose = fs::read_to_string(dip.join("docker-compose.yml")).unwrap();
        // NestJS template has postgres; shared template has nginx placeholder
        assert!(
            compose.contains("postgres") || compose.contains("pg"),
            "nestjs compose not applied"
        );
    }

    #[test]
    fn apply_laravel_substitutes_mysql_database() {
        let dip = tempdir().unwrap().path().join(".dip");
        let tmpl = crate::templates::find("laravel").unwrap();
        apply(&dip, Some(tmpl.dir), "shop", "shop.test").unwrap();

        let env = fs::read_to_string(dip.join("default.env")).unwrap();
        assert!(
            env.contains("shop"),
            "PROJECT_NAME not substituted in laravel env"
        );
        assert!(!env.contains("{PROJECT_NAME}"), "placeholder still present");
    }

    #[test]
    fn apply_node_multi_template_adds_apps_and_env_files() {
        let dip = tempdir().unwrap().path().join(".dip");
        let tmpl = crate::templates::find("node-multi").unwrap();
        apply(&dip, Some(tmpl.dir), "workspace", "workspace.test").unwrap();

        let compose = fs::read_to_string(dip.join("docker-compose.yml")).unwrap();
        assert!(compose.contains("app-web"), "web app service missing");
        assert!(compose.contains("app-api"), "api app service missing");
        assert!(compose.contains("app-worker"), "worker app service missing");
        assert!(
            compose.contains("postgres-default"),
            "shared postgres service missing"
        );
        assert!(dip.join("env/base.env").exists(), "base env missing");
        assert!(dip.join("env/web.env").exists(), "web env missing");
        assert!(dip.join("commands/pnpm").exists(), "pnpm command missing");
    }
}
