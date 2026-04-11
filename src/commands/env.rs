use anyhow::Result;
use colored::Colorize;

use crate::project::ProjectConfig;
use crate::utils::output::Output;

pub fn run(no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let project = ProjectConfig::load()?;
    let env = project.get_env();

    // Show only project-specific vars: those from .env + DIP-injected ones
    let dip_keys = [
        "PROJECT_NAME",
        "PROJECT_ROOT",
        "DIP_DIR",
        "ENV_FILE",
        "HOST_UID",
        "HOST_GID",
        "COMPOSE_PROJECT_NAME",
    ];

    // Read .env keys directly to show what the user defined
    let env_file_keys = read_env_keys(&project.env_file);

    // Combine: .env keys + dip-injected keys, sorted
    let mut keys: Vec<&str> = env_file_keys
        .iter()
        .map(String::as_str)
        .chain(dip_keys.iter().copied())
        .collect();
    keys.sort_unstable();
    keys.dedup();

    let title = format!("env  {}", project.env_file.display());
    out.section(&title, || {
        for key in keys {
            if let Some(val) = env.get(key) {
                let val = val.trim_matches('"').trim_matches('\'');
                println!("  {:30} {}", key.cyan(), val.dimmed());
            }
        }
    });
    Ok(())
}

fn read_env_keys(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let l = l.trim();
            if l.is_empty() || l.starts_with('#') {
                return None;
            }
            l.split_once('=').map(|(k, _)| k.trim().to_string())
        })
        .collect()
}
