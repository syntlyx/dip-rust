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

/// Build the argv for `compose exec`.
///
/// Multiple args are passed straight through to the container — no
/// intermediate `sh -c`. Joining args and re-running them through a shell
/// loses the original quoting (the caller's shell already split them), so
/// anything containing shell metacharacters — `()`, `*`, `;`, quotes — gets
/// re-interpreted and breaks. A classic offender is SQL:
/// `psql -c "SELECT count(*) FROM t WHERE id IN (1,2)"`.
///
/// A single arg containing whitespace is the opposite intent: the caller
/// quoted a whole command line (`dip exec app "phpunit -c conf tests/*"`)
/// and expects shell semantics — word splitting, globs expanded inside the
/// container — so it runs via `sh -c`.
///
/// `-i` keeps stdin open; we omit `-t` because exec is often called from
/// scripts without a TTY.
fn build_exec_args(service: &str, command: &[String]) -> Vec<String> {
    let mut args: Vec<String> = vec!["exec".into(), "-i".into(), service.into()];
    if command.len() == 1 && command[0].contains(char::is_whitespace) {
        args.extend(["sh".into(), "-c".into(), command[0].clone()]);
    } else {
        args.extend_from_slice(command);
    }
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
    fn wraps_single_quoted_command_line_in_sh_c() {
        let line = "/var/www/vendor/bin/phpunit -c docroot/core/phpunit.xml.dist docroot/modules/custom/*/tests/src/Unit";
        let args = build_exec_args("app", &cmd(&[line]));
        // One whitespace-containing arg means the caller quoted a full command
        // line — run it through a shell so words split and globs expand.
        assert_eq!(args, cmd(&["exec", "-i", "app", "sh", "-c", line]));
    }

    #[test]
    fn single_arg_without_whitespace_runs_directly() {
        let args = build_exec_args("app", &cmd(&["bash"]));
        assert_eq!(args, cmd(&["exec", "-i", "app", "bash"]));
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
