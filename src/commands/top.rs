use anyhow::Result;

use crate::project::ProjectConfig;
use crate::runtime::Runtime;
use crate::utils::output::Output;

pub fn run(service: Option<String>, verbose: bool, no_color: bool) -> Result<()> {
    Output::new(no_color);
    Runtime::check_daemon()?;

    let project = ProjectConfig::load()?;
    let rt = Runtime::new(project, verbose, no_color);

    if let Some(ref svc) = service {
        rt.compose_stream(&["top", svc])?;
    } else {
        rt.compose_stream(&["top"])?;
    }
    Ok(())
}
