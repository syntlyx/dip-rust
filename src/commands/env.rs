use crate::utils::style::Stylize;
use anyhow::Result;
use std::collections::BTreeSet;

use crate::project::ProjectConfig;
use crate::utils::env;
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

pub fn run_diff(show_values: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let project = ProjectConfig::load()?;
    let default_env = project.dip_dir.join("default.env");

    let default_vars = env::parse_env_file(&default_env)?;
    let local_vars = env::parse_env_file(&project.env_file)?;

    let default_keys: BTreeSet<&str> = default_vars.keys().map(String::as_str).collect();
    let local_keys: BTreeSet<&str> = local_vars.keys().map(String::as_str).collect();

    let missing: Vec<&str> = default_keys.difference(&local_keys).copied().collect();
    let extra: Vec<&str> = local_keys.difference(&default_keys).copied().collect();
    let changed: Vec<&str> = default_keys
        .intersection(&local_keys)
        .copied()
        .filter(|key| default_vars[*key] != local_vars[*key])
        .collect();
    let empty_overrides: Vec<&str> = default_keys
        .intersection(&local_keys)
        .copied()
        .filter(|key| !default_vars[*key].trim().is_empty() && local_vars[*key].trim().is_empty())
        .collect();

    out.section("env diff", || {
        println!("  {:16} {}", "default:".dimmed(), default_env.display());
        println!("  {:16} {}", "local:".dimmed(), project.env_file.display());
        println!();

        print_key_list("missing", &missing, "present in default.env only");
        print_key_list("extra", &extra, "present in .env only");
        print_key_list(
            "empty",
            &empty_overrides,
            "empty in .env but non-empty in default.env",
        );

        if changed.is_empty() {
            println!("  {} {}", "changed".dimmed(), "none".dimmed());
        } else {
            println!(
                "  {} {}",
                "changed".dimmed(),
                format!("{} key(s)", changed.len()).yellow()
            );
            for key in &changed {
                if show_values {
                    println!(
                        "    {} {} {}",
                        (*key).cyan(),
                        sanitize_value(&default_vars[*key]).dimmed(),
                        format!("→ {}", sanitize_value(&local_vars[*key])).yellow()
                    );
                } else {
                    println!("    {}", (*key).cyan());
                }
            }
            if !show_values {
                println!(
                    "    {}",
                    "use --show-values to print changed values".dimmed()
                );
            }
        }

        println!();
        if missing.is_empty() && extra.is_empty() && empty_overrides.is_empty() {
            println!(
                "  {} env files are in sync structurally",
                "✓".green().bold()
            );
        } else {
            println!(
                "  {} review env drift before sharing this project",
                "⚠".yellow()
            );
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

fn print_key_list(title: &str, keys: &[&str], description: &str) {
    if keys.is_empty() {
        println!("  {} {}", title.dimmed(), "none".dimmed());
        return;
    }

    println!(
        "  {} {}",
        title.dimmed(),
        format!("{} key(s) — {description}", keys.len()).yellow()
    );
    for key in keys {
        println!("    {}", (*key).cyan());
    }
}

fn sanitize_value(value: &str) -> String {
    if value.is_empty() {
        "<empty>".to_string()
    } else {
        value.to_string()
    }
}
