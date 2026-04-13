use std::io::BufRead;
use std::process::{Command, Stdio};

use anyhow::Result;

use crate::utils::log_verbose;
use crate::utils::output::{colorize_compose_line, make_spinner};

use super::{BackendCtx, ContainerRuntime};

pub struct DockerRuntime;

impl ContainerRuntime for DockerRuntime {
    fn check_daemon(&self) -> Result<()> {
        let ok = Command::new("docker")
            .args(["info"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            anyhow::bail!("Docker daemon is not running. Start Docker and try again.")
        }
    }

    fn compose_run(&self, ctx: &BackendCtx, args: &[&str], msg: &str) -> Result<()> {
        let compose_file = ctx.project.compose_file.to_string_lossy().into_owned();
        let mut cmd_args = vec!["compose", "-f", compose_file.as_str()];
        cmd_args.extend_from_slice(args);

        log_cmd(ctx.verbose, &cmd_args);

        let pb = make_spinner(msg);

        let mut child = Command::new("docker")
            .args(&cmd_args)
            .envs(ctx.project.get_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let pb_out = pb.clone();
        let nc = ctx.no_color;
        let stdout_thread = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                pb_out.println(colorize_compose_line(&line, nc));
            }
        });

        let pb_err = pb.clone();
        let stderr_thread = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                if !is_env_warning(&line) {
                    pb_err.println(colorize_compose_line(&line, nc));
                }
            }
        });

        let status = child.wait()?;
        if stdout_thread.join().is_err() {
            log_verbose(ctx.verbose, "  [warn] stdout reader thread panicked");
        }
        if stderr_thread.join().is_err() {
            log_verbose(ctx.verbose, "  [warn] stderr reader thread panicked");
        }
        pb.finish_and_clear();

        if !status.success() {
            anyhow::bail!("docker compose command failed");
        }
        Ok(())
    }

    fn compose_stream(&self, ctx: &BackendCtx, args: &[&str]) -> Result<()> {
        let compose_file = ctx.project.compose_file.to_string_lossy().into_owned();
        let mut cmd_args = vec!["compose", "-f", compose_file.as_str()];
        cmd_args.extend_from_slice(args);

        log_cmd(ctx.verbose, &cmd_args);

        let status = Command::new("docker")
            .args(&cmd_args)
            .envs(ctx.project.get_env())
            .status()?;

        let code = status.code().unwrap_or(0);
        if !status.success() && code != 130 {
            anyhow::bail!("docker compose command failed (exit {})", status);
        }
        Ok(())
    }

    fn compose_stream_grep(
        &self,
        ctx: &BackendCtx,
        args: &[&str],
        keywords: &[&str],
    ) -> Result<()> {
        use std::io::BufRead;

        let compose_file = ctx.project.compose_file.to_string_lossy().into_owned();
        let mut cmd_args = vec!["compose", "-f", compose_file.as_str()];
        cmd_args.extend_from_slice(args);

        log_cmd(ctx.verbose, &cmd_args);

        let mut child = Command::new("docker")
            .args(&cmd_args)
            .envs(ctx.project.get_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdout = child.stdout.take().unwrap();
        let nc = ctx.no_color;

        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            let lower = line.to_lowercase();
            if keywords.iter().any(|kw| lower.contains(kw)) {
                println!("{}", colorize_compose_line(&line, nc));
            }
        }

        let status = child.wait()?;
        let code = status.code().unwrap_or(0);
        if !status.success() && code != 130 {
            anyhow::bail!("docker compose command failed (exit {})", status);
        }
        Ok(())
    }

    fn compose_capture(&self, ctx: &BackendCtx, args: &[&str]) -> Result<String> {
        let compose_file = ctx.project.compose_file.to_string_lossy().into_owned();
        let mut cmd_args = vec!["compose", "-f", compose_file.as_str()];
        cmd_args.extend_from_slice(args);

        log_cmd(ctx.verbose, &cmd_args);

        let output = Command::new("docker")
            .args(&cmd_args)
            .envs(ctx.project.get_env())
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("docker compose failed: {}", stderr.trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn raw_capture(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("docker").args(args).output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("docker command failed: {}", stderr.trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn raw_stream(&self, args: &[&str]) -> Result<()> {
        let status = Command::new("docker").args(args).status()?;
        let code = status.code().unwrap_or(0);
        if !status.success() && code != 130 {
            anyhow::bail!("docker command failed (exit {})", status);
        }
        Ok(())
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn log_cmd(verbose: bool, args: &[&str]) {
    log_verbose(verbose, &format!("  docker {}", args.join(" ")));
}

fn is_env_warning(line: &str) -> bool {
    line.contains("variable is not set. Defaulting to a blank string.")
}
