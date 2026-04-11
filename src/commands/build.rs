use anyhow::Result;

use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::output::Output;

pub fn run(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;
    let project = ProjectConfig::load()?;
    let rt = Runtime::new(project, verbose, no_color);

    let msg = match &service {
        Some(s) => format!("Building service '{s}'..."),
        None => "Building all services...".to_string(),
    };

    let mut args = vec!["build"];
    if let Some(ref s) = service {
        args.push(s.as_str());
    }
    rt.compose_run(&args, &msg)?;
    out.success("Build complete");
    Ok(())
}
