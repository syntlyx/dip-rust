use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use include_dir::{Dir, DirEntry};

use crate::project::ProjectConfig;
use crate::templates::{self, TemplateMeta};
use crate::utils::ensure_executable;
use crate::utils::output::Output;

/// Name of the marker file that records which template scaffolded the project.
/// Written by `dip init` and refreshed here so detection is only needed once.
pub(crate) const TEMPLATE_MARKER: &str = ".template";

/// Sibling of `commands/` where the previous version of an updated script is
/// kept. Outside `commands/` so `dip run` never lists backups as scripts.
const BACKUP_DIR: &str = "commands.bak";

pub fn run(template: Option<&str>, dry_run: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let project = ProjectConfig::load()?;
    let summary = update_in(&project.dip_dir, template, dry_run, &out)?;

    println!();
    if summary.added.is_empty() && summary.updated.is_empty() {
        out.success(&format!(
            "All {} scaffolded scripts are up to date",
            summary.unchanged
        ));
    } else if dry_run {
        out.info(&format!(
            "Would add {}, update {} ({} unchanged). Run without --dry-run to apply.",
            summary.added.len(),
            summary.updated.len(),
            summary.unchanged
        ));
    } else {
        out.success(&format!(
            "Added {}, updated {} ({} unchanged)",
            summary.added.len(),
            summary.updated.len(),
            summary.unchanged
        ));
        if !summary.updated.is_empty() {
            out.info(&format!("Previous versions saved under .dip/{BACKUP_DIR}/"));
        }
    }
    Ok(())
}

#[derive(Default, Debug)]
pub struct Summary {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: usize,
}

/// Refresh scaffolded scripts under `<dip_dir>/commands` from the embedded
/// templates. Only files that exist in the shared base or the project's
/// template are touched — custom scripts are never modified or removed.
pub fn update_in(
    dip_dir: &Path,
    template: Option<&str>,
    dry_run: bool,
    out: &Output,
) -> Result<Summary> {
    let tmpl = resolve_template(dip_dir, template, out)?;

    // Desired state: shared base, with the template layered on top
    // (same order as `dip init`, so overlapping files take the template version).
    let mut desired: BTreeMap<&'static str, &'static [u8]> = BTreeMap::new();
    collect_command_files(templates::shared(), &mut desired);
    if let Some(t) = tmpl {
        collect_command_files(t.dir, &mut desired);
    }

    let mut summary = Summary::default();
    for (rel, content) in &desired {
        let dest = dip_dir.join(rel);
        match fs::read(&dest) {
            Ok(existing) if existing == *content => summary.unchanged += 1,
            Ok(_) => {
                if !dry_run {
                    // Keep the old version: .dip/commands/foo → .dip/commands.bak/foo
                    let rel_in_commands = Path::new(rel)
                        .strip_prefix("commands")
                        .unwrap_or(Path::new(rel));
                    let backup = dip_dir.join(BACKUP_DIR).join(rel_in_commands);
                    if let Some(parent) = backup.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::copy(&dest, &backup)?;
                    write_script(&dest, content)?;
                }
                println!("  ~ {rel}");
                summary.updated.push((*rel).to_string());
            }
            Err(_) => {
                if !dry_run {
                    write_script(&dest, content)?;
                }
                println!("  + {rel}");
                summary.added.push((*rel).to_string());
            }
        }
    }

    // Remember the template so the next run skips detection.
    if !dry_run && let Some(t) = tmpl {
        fs::write(dip_dir.join(TEMPLATE_MARKER), t.name)?;
    }

    Ok(summary)
}

fn write_script(dest: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(dest, content)?;
    if content.starts_with(b"#!") {
        ensure_executable(dest)?;
    }
    Ok(())
}

/// Figure out which template the project was scaffolded from:
/// explicit argument → `.dip/.template` marker → detection by matching the
/// project's command scripts against each template's.
fn resolve_template(
    dip_dir: &Path,
    template: Option<&str>,
    out: &Output,
) -> Result<Option<&'static TemplateMeta>> {
    if let Some(name) = template {
        let t = templates::find(name)?;
        out.info(&format!("Template: {}", t.name));
        return Ok(Some(t));
    }

    if let Ok(name) = fs::read_to_string(dip_dir.join(TEMPLATE_MARKER)) {
        match templates::find(name.trim()) {
            Ok(t) => {
                out.info(&format!(
                    "Template: {} (from .dip/{TEMPLATE_MARKER})",
                    t.name
                ));
                return Ok(Some(t));
            }
            Err(_) => out.warning(&format!(
                "Marker .dip/{TEMPLATE_MARKER} names unknown template '{}'; detecting instead",
                name.trim()
            )),
        }
    }

    let candidates = detect_templates(dip_dir);
    match candidates.as_slice() {
        [] => {
            out.info("Template: none detected — updating shared scripts only");
            Ok(None)
        }
        [t] => {
            out.info(&format!("Template: {} (auto-detected)", t.name));
            Ok(Some(t))
        }
        many => {
            // Several templates match. If they'd all write identical files
            // (e.g. templates whose only command is the same pnpm wrapper),
            // the choice doesn't matter; otherwise ask the user to pick.
            let first_files = command_files(many[0]);
            if many.iter().all(|t| command_files(t) == first_files) {
                out.info(&format!("Template: {} (auto-detected)", many[0].name));
                return Ok(Some(many[0]));
            }
            let names: Vec<&str> = many.iter().map(|t| t.name).collect();
            anyhow::bail!(
                "Multiple templates match this project ({}). Pass one explicitly: dip update-commands <template>",
                names.join(", ")
            )
        }
    }
}

/// Templates with the highest number of command scripts present in the project
/// (only templates with at least one match).
fn detect_templates(dip_dir: &Path) -> Vec<&'static TemplateMeta> {
    let mut best: Vec<&'static TemplateMeta> = Vec::new();
    let mut best_score = 0usize;
    for t in templates::TEMPLATES {
        let score = command_files(t)
            .keys()
            .filter(|rel| dip_dir.join(rel).exists())
            .count();
        if score == 0 {
            continue;
        }
        match score.cmp(&best_score) {
            std::cmp::Ordering::Greater => {
                best_score = score;
                best = vec![t];
            }
            std::cmp::Ordering::Equal => best.push(t),
            std::cmp::Ordering::Less => {}
        }
    }
    best
}

fn command_files(t: &'static TemplateMeta) -> BTreeMap<&'static str, &'static [u8]> {
    let mut map = BTreeMap::new();
    collect_command_files(t.dir, &mut map);
    map
}

/// Collect every file under the embedded `commands/` subtree, keyed by its
/// path relative to the template root (e.g. `commands/utils/color.sh`).
fn collect_command_files(
    dir: &'static Dir<'static>,
    map: &mut BTreeMap<&'static str, &'static [u8]>,
) {
    let Some(commands) = dir.get_dir("commands") else {
        return;
    };
    collect_files(commands, map);
}

fn collect_files(dir: &'static Dir<'static>, map: &mut BTreeMap<&'static str, &'static [u8]>) {
    for entry in dir.entries() {
        match entry {
            DirEntry::Dir(sub) => collect_files(sub, map),
            DirEntry::File(file) => {
                if let Some(path) = file.path().to_str() {
                    map.insert(path, file.contents());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn scaffold(template: &str) -> TempDir {
        let tmp = TempDir::new().unwrap();
        let tmpl = crate::templates::find(template).unwrap();
        crate::commands::init::apply(tmp.path(), Some(tmpl.dir), "proj", "proj.test").unwrap();
        tmp
    }

    fn quiet() -> Output {
        Output::new(true)
    }

    #[test]
    fn fresh_project_is_up_to_date() {
        let tmp = scaffold("drupal");
        let summary = update_in(tmp.path(), None, false, &quiet()).unwrap();
        assert!(summary.added.is_empty(), "added: {:?}", summary.added);
        assert!(summary.updated.is_empty(), "updated: {:?}", summary.updated);
        assert!(summary.unchanged > 0);
        // Detection result is persisted for the next run.
        assert_eq!(
            fs::read_to_string(tmp.path().join(TEMPLATE_MARKER)).unwrap(),
            "drupal"
        );
    }

    #[test]
    fn restores_modified_and_missing_scripts() {
        let tmp = scaffold("drupal");
        let drush = tmp.path().join("commands/drush");
        let composer = tmp.path().join("commands/composer");
        fs::write(&drush, "#!/bin/sh\necho old broken wrapper\n").unwrap();
        fs::remove_file(&composer).unwrap();

        let summary = update_in(tmp.path(), None, false, &quiet()).unwrap();

        assert_eq!(summary.updated, vec!["commands/drush".to_string()]);
        assert_eq!(summary.added, vec!["commands/composer".to_string()]);
        let restored = fs::read_to_string(&drush).unwrap();
        assert!(restored.contains("\"$@\""), "restored: {restored}");
        assert!(composer.exists());
        // Old version is backed up outside commands/.
        let backup = fs::read_to_string(tmp.path().join("commands.bak/drush")).unwrap();
        assert!(backup.contains("old broken wrapper"));
    }

    #[test]
    fn custom_scripts_are_untouched() {
        let tmp = scaffold("drupal");
        let custom = tmp.path().join("commands/deploy");
        fs::write(&custom, "#!/bin/sh\necho custom\n").unwrap();

        update_in(tmp.path(), None, false, &quiet()).unwrap();

        assert_eq!(
            fs::read_to_string(&custom).unwrap(),
            "#!/bin/sh\necho custom\n"
        );
    }

    #[test]
    fn dry_run_writes_nothing() {
        let tmp = scaffold("drupal");
        let drush = tmp.path().join("commands/drush");
        fs::write(&drush, "#!/bin/sh\necho old\n").unwrap();

        let summary = update_in(tmp.path(), None, true, &quiet()).unwrap();

        assert_eq!(summary.updated, vec!["commands/drush".to_string()]);
        assert_eq!(fs::read_to_string(&drush).unwrap(), "#!/bin/sh\necho old\n");
        assert!(!tmp.path().join("commands.bak").exists());
        assert!(!tmp.path().join(TEMPLATE_MARKER).exists());
    }

    #[test]
    fn explicit_template_overrides_detection() {
        let tmp = scaffold("drupal");
        let err = update_in(tmp.path(), Some("nope"), false, &quiet()).unwrap_err();
        assert!(err.to_string().contains("Unknown template"));
    }
}
