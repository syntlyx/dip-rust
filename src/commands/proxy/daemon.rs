//! Proxy daemon lifecycle — start, stop, logs, status, serve.
//!
//! Also owns the low-level helpers (PID file, log file, port conflict check)
//! shared with the other proxy submodules.

use std::path::PathBuf;

use anyhow::Result;
use colored::Colorize;

use crate::dirs;
use crate::proxy::config;
use crate::utils::output::Output;

// ─── public commands ──────────────────────────────────────────────────────────

/// `dip proxy start`
pub fn run_start(verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);

    if let Some(pid) = daemon_pid() {
        out.info(&format!("Proxy already running (PID {pid})"));
        return Ok(());
    }

    let cfg = config::load().unwrap_or_default();
    check_port_conflicts(&cfg, &out)?;

    // Auto-init on first run
    if !crate::proxy::certs::srv_cert_path().exists() {
        out.info("First run — initialising proxy...");
        super::setup::run_init(no_color)?;
    }

    spawn_daemon(&out, verbose)
}

/// `dip proxy stop`
pub fn run_stop(no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let Some(pid) = daemon_pid() else {
        out.info("Proxy is not running");
        return Ok(());
    };
    out.info(&format!("Stopping proxy (PID {pid})..."));
    let killed = std::process::Command::new("kill")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if killed {
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if daemon_pid().is_none() {
                break;
            }
        }
    }
    let _ = std::fs::remove_file(pid_file());
    out.success("Proxy stopped");
    Ok(())
}

/// `dip proxy logs`
pub fn run_logs(lines: usize, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let log = log_file();

    if !log.exists() {
        out.info("No proxy log found — start the proxy first with `dip proxy start`");
        return Ok(());
    }

    out.info(&format!("Tailing {} (Ctrl+C to exit)...", log.display()));

    let status = std::process::Command::new("tail")
        .args(["-n", &lines.to_string(), "-f"])
        .arg(&log)
        .status();

    match status {
        Ok(s) if s.code() == Some(130) || s.success() => {}
        Ok(s) => anyhow::bail!("tail exited with {s}"),
        Err(e) => anyhow::bail!("failed to run tail: {e}"),
    }

    Ok(())
}

/// `dip proxy status`
pub fn run_status(no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    match daemon_pid() {
        Some(pid) => {
            let cfg = config::load()?;
            out.section("proxy", || {
                println!(
                    "  {:18} {} (PID {})",
                    "Status:".dimmed(),
                    "running".green().bold(),
                    pid
                );
                println!(
                    "  {:18} :{} → :{}",
                    "Ports:".dimmed(),
                    cfg.http_port,
                    cfg.https_port
                );
                println!("  {:18} {} route(s)", "Routes:".dimmed(), cfg.routes.len());
                println!("  {:18} {}", "Logs:".dimmed(), log_file().display());
            });
        }
        None => out.warning("Proxy is not running"),
    }
    Ok(())
}

/// `dip proxy serve` — runs the async proxy server in the foreground.
/// Called internally by the daemon spawner via `dip proxy serve`.
pub fn run_serve(no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let cfg = config::load()?;
    out.info(&format!(
        "Proxy: HTTP :{} → HTTPS :{}, {} route(s)",
        cfg.http_port,
        cfg.https_port,
        cfg.routes.len()
    ));
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(crate::proxy::server::run(cfg))
}

// ─── shared helpers (pub(super) — used by routes.rs and setup.rs) ─────────────

pub(super) fn pid_file() -> PathBuf {
    dirs::proxy_dir().join("proxy.pid")
}

pub(super) fn log_file() -> PathBuf {
    dirs::proxy_dir().join("proxy.log")
}

/// Return the PID of the running proxy daemon, or `None` if it is not running.
pub fn daemon_pid() -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(pid_file())
        .ok()
        .and_then(|s| s.trim().parse().ok())?;
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if alive { Some(pid) } else { None }
}

/// Send SIGHUP to the proxy daemon to trigger a hot-reload of routes.
pub(super) fn sighup_daemon() {
    if let Some(pid) = daemon_pid() {
        let _ = std::process::Command::new("kill")
            .args(["-HUP", &pid.to_string()])
            .status();
    }
}

/// Spawn the proxy daemon in the background, writing PID and output to files.
pub(super) fn spawn_daemon(out: &Output, verbose: bool) -> Result<()> {
    let log_path = log_file();
    let log_dir = log_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Invalid log path: {}", log_path.display()))?;
    std::fs::create_dir_all(log_dir)?;

    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let child = std::process::Command::new(std::env::current_exe()?)
        .args(["proxy", "serve"])
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()?;

    let pid = child.id();
    std::fs::write(pid_file(), pid.to_string())?;

    std::thread::sleep(std::time::Duration::from_millis(400));

    if daemon_pid().is_some() {
        out.success(&format!("Proxy started (PID {pid})"));
        if verbose {
            out.info(&format!("Logs: {}", log_path.display()));
        }
    } else {
        out.error("Proxy failed to start — check logs:");
        println!("  {}", log_path.display());
    }
    Ok(())
}

// ─── port conflict detection ──────────────────────────────────────────────────

/// Check whether ports needed by the proxy are already in use.
/// HTTP/HTTPS conflicts are hard errors; DNS conflict is a warning only.
fn check_port_conflicts(cfg: &config::ProxyConfig, out: &Output) -> Result<()> {
    let mut blocking = vec![];

    for (port, proto, required) in [
        (cfg.http_port, "tcp", true),
        (cfg.https_port, "tcp", true),
        (cfg.dns_port, "udp", false),
    ] {
        if let Some(proc) = port_in_use(port, proto) {
            if required {
                blocking.push((port, proc));
            } else {
                out.warning(&format!(
                    "DNS port {port} is in use by {proc} — built-in DNS server will not start"
                ));
            }
        }
    }

    if blocking.is_empty() {
        return Ok(());
    }

    let mut msg = String::from("Port conflict — cannot start proxy:\n");
    for (port, proc) in &blocking {
        msg.push_str(&format!("\n  :{port} is used by {}", proc.yellow()));
    }
    msg.push_str("\n\nOptions:");
    if blocking
        .iter()
        .any(|(p, _)| *p == cfg.http_port || *p == cfg.https_port)
    {
        msg.push_str("\n  • Stop the conflicting process and retry");
        msg.push_str("\n  • Or run with sudo if another dip/nginx is holding the port");
    }

    anyhow::bail!("{msg}")
}

/// Returns `Some("processname (PID 1234)")` if something is listening on `port`.
fn port_in_use(port: u16, proto: &str) -> Option<String> {
    let filter = format!("{proto}:{port}");
    let mut args = vec!["-i", &filter, "-n", "-P", "-F", "cpn"];
    if proto == "tcp" {
        args.extend(["-sTCP:LISTEN"]);
    }

    let output = std::process::Command::new("lsof")
        .args(&args)
        .output()
        .ok()?;

    if !output.status.success() || output.stdout.is_empty() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut pid = String::new();
    let mut cmd = String::new();

    for line in text.lines() {
        match line.chars().next() {
            Some('p') => pid = line[1..].to_string(),
            Some('c') => cmd = line[1..].to_string(),
            _ => {}
        }
        if !pid.is_empty() && !cmd.is_empty() {
            break;
        }
    }

    if cmd.is_empty() {
        return None;
    }

    Some(if pid.is_empty() {
        cmd
    } else {
        format!("{cmd} (PID {pid})")
    })
}
