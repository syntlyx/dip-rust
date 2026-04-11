use anyhow::Result;
use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::cli::Cli;
use crate::dirs;

pub fn run(shell: Shell) -> Result<()> {
    let ext = match shell {
        Shell::Zsh => "zsh",
        Shell::Bash => "bash",
        Shell::Fish => "fish",
        _ => "sh",
    };

    let dir = dirs::config_dir().join("completions");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("dip.{ext}"));

    let mut buf = Vec::new();
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut buf);
    std::fs::write(&path, &buf)?;

    println!("Completions saved to: {}", path.display());
    println!();

    match shell {
        Shell::Zsh => println!("Add to ~/.zshrc:\n  source \"{}\"", path.display()),
        Shell::Bash => println!("Add to ~/.bashrc:\n  source \"{}\"", path.display()),
        Shell::Fish => println!(
            "Or symlink to fish completions dir:\n  ln -s \"{}\" ~/.config/fish/completions/dip.fish",
            path.display()
        ),
        _ => println!("Source the file in your shell rc: {}", path.display()),
    }

    Ok(())
}
