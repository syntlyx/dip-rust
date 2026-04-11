use std::io::BufRead;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};

use crate::project::ProjectConfig;
use crate::utils::output::colorize_compose_line;

use super::ContainerRuntime;

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

    fn compose_run(
        &self,
        project: &ProjectConfig,
        args: &[&str],
        msg: &str,
        verbose: bool,
        no_color: bool,
    ) -> Result<()> {
        let compose_file = project.compose_file.to_string_lossy().into_owned();
        let mut cmd_args = vec!["compose", "-f", compose_file.as_str()];
        cmd_args.extend_from_slice(args);

        if verbose {
            eprintln!("  docker {}", cmd_args.join(" "));
        }

        let pb = spinner(msg, no_color);

        let mut child = Command::new("docker")
            .args(&cmd_args)
            .envs(project.get_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();

        let pb_out = pb.clone();
        let nc = no_color;
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
        stdout_thread.join().ok();
        stderr_thread.join().ok();
        pb.finish_and_clear();

        if !status.success() {
            anyhow::bail!("docker compose command failed");
        }
        Ok(())
    }

    fn compose_stream(&self, project: &ProjectConfig, args: &[&str], verbose: bool) -> Result<()> {
        let compose_file = project.compose_file.to_string_lossy().into_owned();
        let mut cmd_args = vec!["compose", "-f", compose_file.as_str()];
        cmd_args.extend_from_slice(args);

        if verbose {
            eprintln!("  docker {}", cmd_args.join(" "));
        }

        let status = Command::new("docker")
            .args(&cmd_args)
            .envs(project.get_env())
            .status()?;

        let code = status.code().unwrap_or(0);
        if !status.success() && code != 130 {
            anyhow::bail!("docker compose command failed (exit {})", status);
        }
        Ok(())
    }

    fn compose_capture(
        &self,
        project: &ProjectConfig,
        args: &[&str],
        verbose: bool,
    ) -> Result<String> {
        let compose_file = project.compose_file.to_string_lossy().into_owned();
        let mut cmd_args = vec!["compose", "-f", compose_file.as_str()];
        cmd_args.extend_from_slice(args);

        if verbose {
            eprintln!("  docker {}", cmd_args.join(" "));
        }

        let output = Command::new("docker")
            .args(&cmd_args)
            .envs(project.get_env())
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
        // Exit code 130 = Ctrl+C (SIGINT) — treat as success so callers don't
        // print a spurious error after the user intentionally quits.
        let code = status.code().unwrap_or(0);
        if !status.success() && code != 130 {
            anyhow::bail!("docker command failed (exit {})", status);
        }
        Ok(())
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn spinner(msg: &str, no_color: bool) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    if !no_color {
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap(),
        );
    }
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(80));
    pb
}

fn is_env_warning(line: &str) -> bool {
    line.contains("variable is not set. Defaulting to a blank string.")
}
