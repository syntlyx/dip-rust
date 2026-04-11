use anyhow::Result;

use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::confirm;
use crate::utils::output::Output;

pub fn run(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    let target = match &service {
        Some(s) => format!("service '{s}'"),
        None => "all project containers".to_string(),
    };

    out.warning(&format!("This will remove {target}."));
    if !confirm("Continue? [y/N]: ")? {
        out.info("Aborted.");
        return Ok(());
    }

    let project = ProjectConfig::load()?;
    let rt = Runtime::new(project, verbose, no_color);

    if let Some(ref svc) = service {
        rt.compose_run(
            &["rm", "-f", "-s", svc.as_str()],
            &format!("Removing {target}..."),
        )?;
    } else {
        rt.compose_run(
            &["down", "--remove-orphans"],
            &format!("Removing {target}..."),
        )?;
    }

    out.success(&format!("Removed {target}"));
    Ok(())
}
