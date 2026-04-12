use anyhow::Result;

use crate::runtime::Runtime;
use crate::utils::confirm;
use crate::utils::output::Output;

pub fn run(volumes: bool, all: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    Runtime::check_daemon()?;

    let mut what = vec![
        "stopped containers",
        "unused networks",
        if all {
            "all unused images"
        } else {
            "dangling images"
        },
        "build cache",
    ];
    if volumes {
        what.push("unused volumes (including named db_data etc.)");
    }

    out.warning("This will remove ALL of the following Docker resources system-wide:");
    for item in &what {
        println!("  • {item}");
    }
    println!();

    if !confirm("Continue? [y/N]: ")? {
        out.info("Aborted.");
        return Ok(());
    }

    // Step 1: remove stopped containers so their volume references are freed
    run_docker(
        &["container", "prune", "-f"],
        &out,
        "Removing stopped containers...",
    );

    // Step 2: build cache + networks (system prune without image flags — images handled separately)
    run_docker(&["builder", "prune", "-f"], &out, "Pruning build cache...");
    run_docker(
        &["network", "prune", "-f"],
        &out,
        "Pruning unused networks...",
    );

    // Step 3: images — always prune dangling, add -a for all unused
    let mut img_args = vec!["image", "prune", "-f"];
    if all {
        img_args.push("-a");
    }
    run_docker(&img_args, &out, "Pruning images...");

    // Step 4: named volumes require `docker volume prune --all` (Docker 23+)
    if volumes {
        run_docker(
            &["volume", "prune", "-f", "--all"],
            &out,
            "Pruning unused volumes...",
        );
    }

    out.success("Docker system pruned");
    Ok(())
}

fn run_docker(args: &[&str], out: &Output, msg: &str) {
    let spinner = out.spinner(msg);
    let _ = std::process::Command::new("docker")
        .args(args)
        .stdout(std::process::Stdio::null())
        .status();
    spinner.finish_and_clear();
}
