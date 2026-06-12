use crate::utils::style::Stylize;
use anyhow::Result;

use crate::cli::RuntimeChoice;
use crate::project::ProjectConfig;
use crate::runtime;
use crate::utils::output::Output;

pub fn run(runtime: RuntimeChoice, project: bool, _global: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let project_config = if project {
        Some(ProjectConfig::load()?)
    } else {
        None
    };

    match runtime {
        RuntimeChoice::Apple => {
            set_runtime(project_config.as_ref(), Some("apple"))?;
            out.success(&format!("Runtime: {}", "apple".green().bold()));
        }
        RuntimeChoice::Docker => {
            set_runtime(project_config.as_ref(), Some("docker"))?;
            out.success(&format!("Runtime: {}", "docker".green().bold()));
        }
        RuntimeChoice::Auto => {
            set_runtime(project_config.as_ref(), None)?;
            out.success("Runtime override cleared");
        }
    }

    let config_path = project_config
        .as_ref()
        .map(runtime::project_runtime_path)
        .unwrap_or_else(runtime::global_runtime_path);
    let scope = if project_config.is_some() {
        "project"
    } else {
        "global"
    };
    println!("  {} {}", "scope:".dimmed(), scope);
    println!("  {} {}", "config:".dimmed(), config_path.display());
    println!(
        "  {} {}",
        "override:".dimmed(),
        "DIP_RUNTIME=apple|docker".dimmed()
    );

    Ok(())
}

fn set_runtime(project: Option<&ProjectConfig>, runtime: Option<&str>) -> Result<()> {
    if let Some(project) = project {
        runtime::set_project_runtime(project, runtime)
    } else {
        runtime::set_global_runtime(runtime)
    }
}
