use anyhow::Result;

use crate::runtime;
use crate::utils::confirm;
use crate::utils::output::Output;

pub fn run(volumes: bool, all: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let backend = runtime::active_backend();
    backend.check_daemon()?;

    out.warning(&format!(
        "This will remove ALL of the following {} resources system-wide:",
        runtime_label(backend.name())
    ));
    for item in backend.prune_plan(volumes, all) {
        println!("  • {item}");
    }
    println!();

    if !confirm("Continue? [y/N]: ")? {
        out.info("Aborted.");
        return Ok(());
    }

    backend.prune_system(volumes, all, &out)?;
    out.success(&format!("{} system pruned", runtime_label(backend.name())));
    Ok(())
}

fn runtime_label(runtime: &str) -> &'static str {
    if runtime == "apple" {
        "Apple Container"
    } else {
        "Docker"
    }
}
