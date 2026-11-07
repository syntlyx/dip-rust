use anyhow::Result;

use crate::commands::ctx::Ctx;

/// Open an interactive shell in `service`. Returns the shell's exit code.
pub fn run_shell(service: &str, shell_type: &str, verbose: bool, no_color: bool) -> Result<i32> {
    let ctx = Ctx::load(verbose, no_color)?;

    ctx.out
        .info(&format!("Opening {shell_type} shell in '{service}'..."));

    let code = ctx
        .rt
        .compose_stream(&["exec", "-it", service, shell_type], true)?;

    // 126/127 mean "could not be run" — but the shell reports its *own* last
    // command the same way, so a session ending on a typo looks identical to a
    // missing shell. Confirm before replacing it, or `dip shell app`, a
    // mistyped command and Ctrl-D would silently drop the user into a second
    // shell. The check only runs on that rare exit code, so the normal path
    // pays nothing.
    if matches!(code, 126 | 127) && shell_type != "sh" && !has_binary(&ctx, service, shell_type)? {
        ctx.out
            .warning(&format!("{shell_type} not found, falling back to sh"));
        return ctx.rt.compose_stream(&["exec", "-it", service, "sh"], true);
    }
    Ok(code)
}

/// Whether `binary` is runnable inside the service's container.
fn has_binary(ctx: &Ctx, service: &str, binary: &str) -> Result<bool> {
    let probe = format!("command -v {binary} >/dev/null 2>&1");
    let code = ctx
        .rt
        .compose_stream(&["exec", "-i", service, "sh", "-c", &probe], true)?;
    Ok(code == 0)
}

/// Run a command in `service`. Returns the command's exit code.
pub fn run_exec(
    service: &str,
    workdir: Option<&str>,
    command: &[String],
    verbose: bool,
    no_color: bool,
) -> Result<i32> {
    let ctx = Ctx::load(verbose, no_color)?;

    if command.is_empty() {
        anyhow::bail!("Please provide a command to execute");
    }

    let args = build_exec_args(service, workdir, command, interactive_terminal());

    ctx.out.info(&format!(
        "Running '{}' in '{service}'...",
        command.join(" ")
    ));
    ctx.rt
        .compose_stream(&args.iter().map(String::as_str).collect::<Vec<_>>(), true)
}

/// Build the argv for `exec`.
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
/// `-i` keeps stdin open. `-t` is the interesting one: `docker compose exec`
/// allocates a pseudo-TTY *by default* (only `-T` turns it off), while
/// `docker exec` allocates one only when asked. That difference is visible to
/// every tool in the container that colorizes on `isatty(stdout)` — phpunit,
/// phpcs, composer, npm — so `-t` has to be requested explicitly to keep their
/// output in colour.
///
/// It is requested only when *both* stdin and stdout are terminals, because a
/// TTY-enabled exec cannot take piped stdin. Compose rejects that combination
/// outright ("cannot attach stdin to a TTY-enabled container"); asking for the
/// TTY only when it can actually work makes `echo sql | dip exec db psql` run
/// from a terminal instead of failing.
///
/// `workdir` maps to `-w`; the apple runtime shim translates it to
/// `container exec --workdir`.
fn build_exec_args(
    service: &str,
    workdir: Option<&str>,
    command: &[String],
    tty: bool,
) -> Vec<String> {
    let mut args: Vec<String> = vec!["exec".into(), "-i".into()];
    if tty {
        args.push("-t".into());
    }
    if let Some(dir) = workdir {
        args.push("-w".into());
        args.push(dir.into());
    }
    args.push(service.into());
    if command.len() == 1 && command[0].contains(char::is_whitespace) {
        args.extend(["sh".into(), "-c".into(), command[0].clone()]);
    } else {
        args.extend_from_slice(command);
    }
    args
}

/// A TTY is only usable when both ends are one: stdout drives whether the
/// command colorizes, stdin whether the daemon will accept the attach at all.
fn interactive_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn passes_command_through_without_shell_wrapper() {
        let args = build_exec_args("db", None, &cmd(&["psql", "-U", "postgres"]), false);
        assert_eq!(args, cmd(&["exec", "-i", "db", "psql", "-U", "postgres"]));
        // No `sh -c` wrapper that would re-parse the command.
        assert!(!args.iter().any(|a| a == "sh" || a == "-c"));
    }

    #[test]
    fn wraps_single_quoted_command_line_in_sh_c() {
        let line = "/var/www/vendor/bin/phpunit -c docroot/core/phpunit.xml.dist docroot/modules/custom/*/tests/src/Unit";
        let args = build_exec_args("app", None, &cmd(&[line]), false);
        // One whitespace-containing arg means the caller quoted a full command
        // line — run it through a shell so words split and globs expand.
        assert_eq!(args, cmd(&["exec", "-i", "app", "sh", "-c", line]));
    }

    #[test]
    fn single_arg_without_whitespace_runs_directly() {
        let args = build_exec_args("app", None, &cmd(&["bash"]), false);
        assert_eq!(args, cmd(&["exec", "-i", "app", "bash"]));
    }

    #[test]
    fn preserves_sql_argument_with_parentheses_as_single_arg() {
        let sql = "SELECT count(*) FROM users WHERE id IN (1,2,3)";
        let args = build_exec_args(
            "db",
            None,
            &cmd(&["psql", "-U", "postgres", "-c", sql]),
            false,
        );
        // The SQL stays a single argv element — parentheses are never seen by a shell.
        assert_eq!(args.last().unwrap(), sql);
        assert_eq!(
            args,
            cmd(&["exec", "-i", "db", "psql", "-U", "postgres", "-c", sql])
        );
    }

    #[test]
    fn workdir_is_inserted_before_service() {
        let args = build_exec_args(
            "app",
            Some("/var/www/theme"),
            &cmd(&["sass", "scss:css"]),
            false,
        );
        assert_eq!(
            args,
            cmd(&[
                "exec",
                "-i",
                "-w",
                "/var/www/theme",
                "app",
                "sass",
                "scss:css"
            ])
        );
    }

    #[test]
    fn a_terminal_asks_for_a_tty_so_the_command_keeps_its_colors() {
        // On a terminal: `-t`, so phpunit/composer/npm still see isatty(stdout)
        // — `docker compose exec` allocated one implicitly and dropping it
        // silently turned their output monochrome.
        let args = build_exec_args("app", None, &cmd(&["phpunit"]), true);
        assert_eq!(args, cmd(&["exec", "-i", "-t", "app", "phpunit"]));

        // Piped or redirected: no `-t`. A TTY-enabled exec cannot take piped
        // stdin, and there is nothing to colorize for.
        let args = build_exec_args("app", None, &cmd(&["phpunit"]), false);
        assert_eq!(args, cmd(&["exec", "-i", "app", "phpunit"]));
    }

    #[test]
    fn the_tty_flag_lands_before_workdir_and_service() {
        let args = build_exec_args("app", Some("/srv"), &cmd(&["ls"]), true);
        assert_eq!(args, cmd(&["exec", "-i", "-t", "-w", "/srv", "app", "ls"]));
    }
}
