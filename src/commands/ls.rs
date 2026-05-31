use std::path::{Path, PathBuf};

use anyhow::Result;
use colored::Colorize;

use crate::utils::output::Output;

// ─── types ────────────────────────────────────────────────────────────────────

struct DipProject {
    name: String,
    root: PathBuf,
}

struct ProjectStatus {
    project: DipProject,
    /// Number of running containers (0 = stopped)
    running: usize,
}

// ─── entry point ─────────────────────────────────────────────────────────────

pub fn run(root: Option<PathBuf>, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);

    let search_root = root.unwrap_or_else(home_dir);
    out.info(&format!("Scanning {}…", search_root.display()));

    // 1. Discover projects (fast, single-threaded with pruning)
    let projects = scan(&search_root);

    if projects.is_empty() {
        out.info("No dip projects found");
        return Ok(());
    }

    // 2. Check Docker status for all projects in parallel
    let mut statuses: Vec<ProjectStatus> = std::thread::scope(|s| {
        projects
            .iter()
            .map(|p| s.spawn(|| check_status(p)))
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|h| h.join().ok())
            .collect()
    });

    // 3. Sort: running first, then alphabetical
    statuses.sort_by(|a, b| {
        b.running
            .cmp(&a.running)
            .then(a.project.name.cmp(&b.project.name))
    });

    // 4. Display
    let home = home_dir();
    let running_count = statuses.iter().filter(|s| s.running > 0).count();

    // Pre-compute display strings (plain, no color) to measure column widths.
    struct Row {
        name: String,
        path: String,
        status: String,
        running: bool,
    }

    let rows: Vec<Row> = statuses
        .iter()
        .map(|s| {
            let path = s
                .project
                .root
                .strip_prefix(&home)
                .map(|p| format!("~/{}", p.display()))
                .unwrap_or_else(|_| s.project.root.display().to_string());

            let status = if s.running > 0 {
                format!(
                    "{} container{}",
                    s.running,
                    if s.running == 1 { "" } else { "s" }
                )
            } else {
                "stopped".to_string()
            };

            Row {
                name: s.project.name.clone(),
                path,
                status,
                running: s.running > 0,
            }
        })
        .collect();

    let name_w = rows.iter().map(|r| r.name.len()).max().unwrap_or(10);
    let path_w = rows.iter().map(|r| r.path.len()).max().unwrap_or(20);

    out.section(
        &format!(
            "dip projects  ({} found, {} running)",
            statuses.len(),
            running_count
        ),
        || {
            for row in &rows {
                let bullet = if row.running {
                    "●".green().to_string()
                } else {
                    "○".dimmed().to_string()
                };

                // Pad plain strings BEFORE colorizing so ANSI codes don't skew widths.
                let name_padded = format!("{:<name_w$}", row.name);
                let path_padded = format!("{:<path_w$}", row.path);

                let name_col = name_padded.cyan().to_string();
                let path_col = path_padded.dimmed().to_string();
                let status_col = if row.running {
                    row.status.green().to_string()
                } else {
                    row.status.dimmed().to_string()
                };

                println!("  {bullet}  {name_col}  {path_col}  {status_col}");
            }
        },
    );

    Ok(())
}

// ─── filesystem scan ──────────────────────────────────────────────────────────

/// Directories to skip entirely during traversal.
const PRUNE: &[&str] = &[
    // Build artifacts / package managers
    "node_modules",
    "vendor",
    "target",
    ".cargo",
    ".rustup",
    "go",
    ".npm",
    ".yarn",
    ".pnpm-store",
    // VCS
    ".git",
    ".svn",
    // macOS system
    "Library",
    "Applications",
    "Music",
    "Pictures",
    "Movies",
    "Desktop",
    "Downloads",
    "Public",
    // Hidden noise
    ".cache",
    ".local",
    ".docker",
    ".orbstack",
    "OrbStack",
];

const MAX_DEPTH: usize = 7;

fn scan(root: &Path) -> Vec<DipProject> {
    let mut found = Vec::new();
    scan_dir(root, &mut found, 0);
    found
}

fn scan_dir(dir: &Path, found: &mut Vec<DipProject>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }

    // If this directory contains .dip/ — it's a dip project
    let dip_dir = dir.join(".dip");
    if dip_dir.is_dir() && dip_dir.join("docker-compose.yml").exists() {
        if let Some(project) = load_project(dir, &dip_dir) {
            found.push(project);
        }
        // Don't recurse into a dip project's subdirs
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let mut children: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|e| {
            // Use file_type() instead of path.is_dir() — avoids following symlinks.
            // This prevents traversing OrbStack/Docker FUSE mounts that appear as
            // symlinked directory trees.
            let ft = e.file_type().ok()?;
            if !ft.is_dir() {
                return None;
            }

            let name = e.file_name();
            let name = name.to_string_lossy();

            // Skip hidden dirs
            if name.starts_with('.') {
                return None;
            }
            // Skip pruned dirs
            if PRUNE.iter().any(|p| *p == name.as_ref()) {
                return None;
            }

            Some(e.path())
        })
        .collect();

    children.sort(); // deterministic order

    for child in children {
        scan_dir(&child, found, depth + 1);
    }
}

fn load_project(root: &Path, dip_dir: &Path) -> Option<DipProject> {
    let name = read_project_name(dip_dir).unwrap_or_else(|| {
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".to_string())
    });

    Some(DipProject {
        name,
        root: root.to_path_buf(),
    })
}

/// Read PROJECT_NAME from .env or default.env.
fn read_project_name(dip_dir: &Path) -> Option<String> {
    for filename in [".env", "default.env"] {
        let path = dip_dir.join(filename);
        if let Ok(content) = std::fs::read_to_string(&path) {
            for line in content.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("PROJECT_NAME=") {
                    let name = rest.trim().trim_matches('"').trim_matches('\'');
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
        }
    }
    None
}

// ─── runtime status ───────────────────────────────────────────────────────────

fn check_status(project: &DipProject) -> ProjectStatus {
    let running = running_container_count(&project.name);
    ProjectStatus {
        project: DipProject {
            name: project.name.clone(),
            root: project.root.clone(),
        },
        running,
    }
}

fn running_container_count(project_name: &str) -> usize {
    crate::runtime::active_backend().running_count_by_project(project_name)
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/"))
}
