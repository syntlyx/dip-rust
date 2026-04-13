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

    // Generate clap completions
    let mut buf = Vec::new();
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    generate(shell, &mut cmd, name, &mut buf);

    // Append the dip shell hook (PATH auto-activation for .dip/commands/)
    let hook = shell_hook(shell, &path.to_string_lossy());
    buf.extend_from_slice(hook.as_bytes());

    std::fs::write(&path, &buf)?;

    println!("Completions saved to: {}", path.display());
    println!();

    // Determine which rc file to suggest / patch
    let rc_file = shell_rc(shell);

    let source_line = match shell {
        Shell::Fish => format!(
            "# Add to ~/.config/fish/config.fish:\n  source \"{}\"",
            path.display()
        ),
        _ => format!("source \"{}\"", path.display()),
    };

    println!("Add to {rc_file}:");
    println!("  {source_line}");
    println!();

    // Only offer auto-patching for zsh/bash (rc file is a simple text file)
    if matches!(shell, Shell::Zsh | Shell::Bash)
        && prompt_yes(&format!("Add to {} automatically? [y/N]", rc_file))
    {
        patch_rc(&rc_file, &format!("source \"{}\"", path.display()))?;
    }

    Ok(())
}

// ─── shell hook source ────────────────────────────────────────────────────────

fn shell_hook(shell: Shell, completion_path: &str) -> String {
    match shell {
        Shell::Zsh => format!(
            r#"
# dip: create aliases for .dip/commands/ so `migrate` → `dip run migrate`
# Works from any subdirectory — uses `dip root` to find the project root.
autoload -U add-zsh-hook
typeset -ga _dip_active_aliases

_dip_activate() {{
  # Remove aliases registered for the previous project
  for _dip_cmd in "${{_dip_active_aliases[@]}}"; do
    unalias "$_dip_cmd" 2>/dev/null
  done
  _dip_active_aliases=()

  # dip root exits 1 if not in a dip project — suppress error output
  local root
  root=$(dip root 2>/dev/null) || return

  local cmds_dir="$root/.dip/commands"
  [[ -d "$cmds_dir" ]] || return

  for f in "$cmds_dir"/*; do
    [[ -f "$f" ]] || continue
    local name="${{f:t}}"
    alias "${{name}}=dip run ${{name}}"
    _dip_active_aliases+=("$name")
  done
}}
add-zsh-hook chpwd _dip_activate
_dip_activate  # run for the current directory right away
# completions file: {completion_path}
"#
        ),
        Shell::Bash => format!(
            r#"
# dip: create aliases for .dip/commands/ so `migrate` → `dip run migrate`
_dip_active_aliases=()

_dip_activate() {{
  for _dip_cmd in "${{_dip_active_aliases[@]}}"; do
    unalias "$_dip_cmd" 2>/dev/null
  done
  _dip_active_aliases=()

  local root
  root=$(dip root 2>/dev/null) || return

  local cmds_dir="$root/.dip/commands"
  [[ -d "$cmds_dir" ]] || return

  for f in "$cmds_dir"/*; do
    [[ -f "$f" ]] || continue
    local name="${{f##*/}}"
    alias "${{name}}=dip run ${{name}}"
    _dip_active_aliases+=("$name")
  done
}}
PROMPT_COMMAND="_dip_activate${{PROMPT_COMMAND:+; $PROMPT_COMMAND}}"
_dip_activate  # run for the current directory right away
# completions file: {completion_path}
"#
        ),
        Shell::Fish => format!(
            r#"
# dip: create aliases for .dip/commands/ so `migrate` → `dip run migrate`
set -g _dip_active_aliases

function _dip_activate --on-variable PWD
  for _dip_cmd in $_dip_active_aliases
    functions --erase $_dip_cmd
  end
  set -g _dip_active_aliases

  set -l root (dip root 2>/dev/null) or return
  set -l cmds_dir "$root/.dip/commands"
  test -d $cmds_dir or return

  for f in $cmds_dir/*
    test -f $f or continue
    set -l name (basename $f)
    eval "function $name; dip run $name \$argv; end"
    set -ga _dip_active_aliases $name
  end
end
_dip_activate
# completions file: {completion_path}
"#
        ),
        _ => String::new(),
    }
}

// ─── rc file helpers ──────────────────────────────────────────────────────────

fn shell_rc(shell: Shell) -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "~".to_string());
    match shell {
        Shell::Zsh => format!("{home}/.zshrc"),
        Shell::Bash => format!("{home}/.bashrc"),
        Shell::Fish => format!("{home}/.config/fish/config.fish"),
        _ => format!("{home}/.profile"),
    }
}

fn patch_rc(rc_path: &str, line: &str) -> Result<()> {
    let path = std::path::Path::new(rc_path);

    // Check if already present
    if path.exists() {
        let content = std::fs::read_to_string(path)?;
        if content.contains(line) {
            println!("Already present in {rc_path} — nothing to do.");
            return Ok(());
        }
    }

    // Append with a blank line separator
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(f, "\n{line}")?;

    println!("Added to {rc_path}.");
    println!("Restart your shell or run:  source \"{rc_path}\"");
    Ok(())
}

fn prompt_yes(question: &str) -> bool {
    use std::io::Write;
    print!("{question} ");
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return false;
    }
    matches!(input.trim().to_lowercase().as_str(), "y" | "yes")
}
