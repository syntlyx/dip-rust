use anyhow::Result;

use crate::commands::ctx::Ctx;

pub fn run_shell(service: &str, shell_type: &str, verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;

    ctx.out
        .info(&format!("Opening {shell_type} shell in '{service}'..."));

    // Try requested shell, fall back to sh
    let result = ctx
        .rt
        .compose_stream(&["exec", "-it", service, shell_type], true);
    if result.is_err() && shell_type != "sh" {
        ctx.out
            .warning(&format!("{shell_type} not found, falling back to sh"));
        ctx.rt
            .compose_stream(&["exec", "-it", service, "sh"], true)?;
    } else {
        result?;
    }
    Ok(())
}

pub fn run_exec(service: &str, command: &[String], verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;

    if command.is_empty() {
        anyhow::bail!("Please provide a command to execute");
    }

    let args = build_exec_args(service, command);

    ctx.out.info(&format!(
        "Running '{}' in '{service}'...",
        command.join(" ")
    ));
    ctx.rt
        .compose_stream(&args.iter().map(String::as_str).collect::<Vec<_>>(), true)?;
    Ok(())
}

/// Build the argv for `compose exec`, passing the command straight through to
/// the container — no intermediate `sh -c`.
///
/// Joining args and re-running them through a shell loses the original quoting
/// (the caller's shell already split them), so anything containing shell
/// metacharacters — `()`, `*`, `;`, quotes — gets re-interpreted and breaks.
/// A classic offender is SQL: `psql -c "SELECT count(*) FROM t WHERE id IN (1,2)"`.
/// For pipes/redirects/variable expansion, invoke a shell explicitly, e.g.
///   dip exec <service> sh -c "foo | bar > baz"
///
/// `-i` keeps stdin open; we omit `-t` because exec is often called from scripts
/// without a TTY.
fn build_exec_args(service: &str, command: &[String]) -> Vec<String> {
    let mut args: Vec<String> = vec!["exec".into(), "-i".into(), service.into()];
    args.extend_from_slice(command);
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn passes_command_through_without_shell_wrapper() {
        let args = build_exec_args("db", &cmd(&["psql", "-U", "postgres"]));
        assert_eq!(args, cmd(&["exec", "-i", "db", "psql", "-U", "postgres"]));
        // No `sh -c` wrapper that would re-parse the command.
        assert!(!args.iter().any(|a| a == "sh" || a == "-c"));
    }

    #[test]
    fn preserves_sql_argument_with_parentheses_as_single_arg() {
        let sql = "SELECT count(*) FROM users WHERE id IN (1,2,3)";
        let args = build_exec_args("db", &cmd(&["psql", "-U", "postgres", "-c", sql]));
        // The SQL stays a single argv element — parentheses are never seen by a shell.
        assert_eq!(args.last().unwrap(), sql);
        assert_eq!(
            args,
            cmd(&["exec", "-i", "db", "psql", "-U", "postgres", "-c", sql])
        );
    }
}
