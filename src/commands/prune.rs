use anyhow::Result;

use crate::runtime::Runtime;
use crate::utils::confirm;
use crate::utils::output::Output;

pub fn run(no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    out.warning("This will remove ALL unused Docker resources system-wide.");
    if !confirm("Continue? [y/N]: ")? {
        out.info("Aborted.");
        return Ok(());
    }

    let spinner = out.spinner("Pruning unused Docker resources...");
    let status = std::process::Command::new("docker")
        .args(["system", "prune", "-f"])
        .status()?;
    spinner.finish_and_clear();

    if status.success() {
        out.success("Docker system pruned");
    } else {
        out.error("Prune command failed");
    }
    Ok(())
}
