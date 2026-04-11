/// Built-in TLS reverse proxy.
///
/// Workflow:
///   dip proxy init          — generate CA + wildcard cert, install CA in keychain
///   dip start               — containers up + proxy auto-synced from docker labels
///   dip proxy routes        — inspect current routing table
///   dip proxy status        — daemon status
///   sudo dip proxy start    — start daemon manually (ports 80/443 need root)
///
/// Routes are discovered from docker-compose labels:
///   labels:
///     dip.host: "${DOMAIN}:80"   →  "backend.foo.test:80"
///
/// The proxy connects to the container's Docker network IP directly.
/// This works on OrbStack and Linux. On Docker Desktop Mac, containers
/// must publish ports and you should use "127.0.0.1:PORT" as the upstream.
use std::path::PathBuf;

use anyhow::Result;
use colored::Colorize;

use crate::dirs;
use crate::dns;
use crate::project::ProjectConfig;
use crate::proxy::certs;
use crate::proxy::config::{self, ProxyConfig, Route};
use crate::utils::output::Output;

// ─── public command entry points ─────────────────────────────────────────────

/// `dip proxy init` — generate CA + server cert, install CA in system keychain.
pub fn run_init(no_color: bool) -> Result<()> {
    let out = Output::new(no_color);

    std::fs::create_dir_all(dirs::proxy_dir())?;

    // 1. Ensure CA exists
    out.info("Setting up Certificate Authority...");
    certs::ensure_ca()?;
    out.success(&format!("CA: {}", certs::ca_cert_path().display()));

    // 2. Install CA in system keychain (so browsers trust all dip certs)
    out.info("Installing CA in system keychain (may prompt for password)...");
    match certs::install_ca() {
        Ok(true) => out.success("CA installed — all dip HTTPS domains are now trusted"),
        Ok(false) => out.info("CA already trusted"),
        Err(e) => out.warning(&format!(
            "CA install failed: {e}\nInstall manually from: {}",
            certs::ca_cert_path().display()
        )),
    }

    // 3. Generate / refresh server cert using current configured routes.
    //    Re-running `dip proxy init` after routes are synced will regenerate
    //    the cert with the correct SANs rather than falling back to localhost.
    out.info("Generating server certificate...");
    let existing_cfg = config::load().unwrap_or_default();
    let domains: Vec<String> = existing_cfg
        .routes
        .iter()
        .map(|r| r.domain.clone())
        .collect();
    let cert_changed = certs::ensure_server_cert(&domains)?;
    if cert_changed {
        out.success(&format!(
            "Cert regenerated: {}",
            certs::srv_cert_path().display()
        ));
    } else {
        out.success(&format!("Cert: {}", certs::srv_cert_path().display()));
    }

    // 4. Write default config if it doesn't exist
    if !config::config_path().exists() {
        config::save(&ProxyConfig::default())?;
        out.success(&format!("Config: {}", config::config_path().display()));
    }

    // 5. Optionally configure DNS
    println!();
    if prompt_yes(
        "Set up DNS for *.test automatically? (skip if you use Pi-hole or handle DNS yourself) [y/N]",
    ) {
        out.info("Setting up DNS for *.test...");
        if let Err(e) = dns::setup("test", &out) {
            out.warning(&format!("DNS setup failed: {e}"));
        }
    } else {
        out.info("Skipping DNS setup — make sure *.test resolves to 127.0.0.1");
    }

    println!();
    out.info("Next: run `dip start` inside a project — routes will be set up automatically.");
    Ok(())
}

/// `dip proxy start` — start the daemon.
pub fn run_start(verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);

    if let Some(pid) = daemon_pid() {
        out.info(&format!("Proxy already running (PID {pid})"));
        return Ok(());
    }

    // Init if never done
    if !certs::srv_cert_path().exists() {
        out.info("First run — initialising proxy...");
        run_init(no_color)?;
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

    // tail -n {lines} -f — show last N lines then follow
    let status = std::process::Command::new("tail")
        .arg("-n")
        .arg(lines.to_string())
        .arg("-f")
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

/// `dip proxy routes`
pub fn run_routes(no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let cfg = config::load()?;
    out.section("proxy routes", || {
        if cfg.routes.is_empty() {
            println!(
                "  {}",
                "no routes — run `dip start` inside a project to populate".dimmed()
            );
        } else {
            for r in &cfg.routes {
                let kind = if r.domain.contains('*') {
                    "wildcard"
                } else {
                    "exact   "
                };
                println!(
                    "  {} {:38} → {}",
                    kind.dimmed(),
                    r.domain.cyan(),
                    r.upstream.green()
                );
            }
        }
    });
    Ok(())
}

/// `dip proxy add <domain> <upstream>`
pub fn run_add(domain: &str, upstream: &str, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let mut cfg = config::load()?;
    cfg.routes.retain(|r| r.domain != domain);
    cfg.routes.push(Route {
        domain: domain.to_string(),
        upstream: upstream.to_string(),
    });
    config::save(&cfg)?;
    out.success(&format!("{} → {}", domain.cyan(), upstream.green()));
    sighup_daemon();
    Ok(())
}

/// `dip proxy remove <domain>`
pub fn run_remove(domain: &str, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let mut cfg = config::load()?;
    let before = cfg.routes.len();
    cfg.routes.retain(|r| r.domain != domain);
    if cfg.routes.len() < before {
        config::save(&cfg)?;
        out.success(&format!("Removed: {domain}"));
        sighup_daemon();
    } else {
        out.warning(&format!("No route for: {domain}"));
    }
    Ok(())
}

/// `dip proxy sync` — discover routes from running docker-compose containers.
/// Also called automatically at the end of `dip start`.
pub fn run_sync(no_color: bool) -> Result<()> {
    let project = ProjectConfig::load()?;
    sync_from_project(&project, no_color)
}

/// Best-effort proxy sync called from lifecycle commands (start, restart).
/// Errors are printed as warnings only when verbose — never propagate.
pub fn apply_sync(project: &ProjectConfig, verbose: bool, no_color: bool) {
    if let Err(e) = sync_from_project(project, no_color)
        && verbose
    {
        Output::new(no_color).warning(&format!("Proxy sync: {e}"));
    }
}

/// Best-effort proxy cleanup called from lifecycle commands (stop).
/// Errors are printed as warnings only when verbose — never propagate.
pub fn apply_unsync(project: &ProjectConfig, verbose: bool, no_color: bool) {
    if let Err(e) = unsync_from_project(project, no_color)
        && verbose
    {
        Output::new(no_color).warning(&format!("Proxy cleanup: {e}"));
    }
}

/// Remove all proxy routes that belong to this project's containers.
/// Works even after containers are stopped — labels are static metadata.
pub fn unsync_from_project(project: &ProjectConfig, no_color: bool) -> Result<()> {
    let domains = discover_project_domains(project)?;
    if domains.is_empty() {
        return Ok(());
    }

    let mut cfg = config::load()?;
    let before = cfg.routes.len();
    cfg.routes.retain(|r| !domains.contains(r.domain.as_str()));

    if cfg.routes.len() < before {
        config::save(&cfg)?;
        let out = Output::new(no_color);
        for d in &domains {
            out.info(&format!("  proxy: removed {}", d.as_str().dimmed()));
        }
        sighup_daemon();
    }

    Ok(())
}

/// Called from `dip start` after containers are up.
/// Best-effort: errors are printed as warnings, never fail the start.
pub fn sync_from_project(project: &ProjectConfig, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);

    // Discover routes from docker labels
    let discovered = match discover_routes(project) {
        Ok(r) => r,
        Err(e) => {
            out.warning(&format!("Proxy route discovery failed: {e}"));
            return Ok(());
        }
    };

    if discovered.is_empty() {
        // No dip.host labels in this project's compose file
        return Ok(());
    }

    // Load current config, replace routes we discovered
    let mut cfg = config::load()?;
    let discovered_domains: std::collections::HashSet<&str> =
        discovered.iter().map(|r| r.domain.as_str()).collect();

    // Keep existing routes that aren't from this project
    cfg.routes
        .retain(|r| !discovered_domains.contains(r.domain.as_str()));
    cfg.routes.extend(discovered.clone());

    // Regenerate cert if new TLDs appeared
    let all_domains: Vec<String> = cfg.routes.iter().map(|r| r.domain.clone()).collect();
    let cert_changed = match certs::ensure_server_cert(&all_domains) {
        Ok(v) => v,
        Err(e) => {
            out.warning(&format!("Cert update failed: {e}"));
            false
        }
    };

    config::save(&cfg)?;

    for r in &discovered {
        out.success(&format!(
            "  proxy: {} → {}",
            r.domain.cyan(),
            r.upstream.green()
        ));
    }

    if !certs::srv_cert_path().exists() {
        // Cert missing entirely — skip proxy management
        return Ok(());
    }

    match (daemon_pid(), cert_changed) {
        (Some(_), false) => {
            // Running, cert unchanged: hot-reload routes via SIGHUP
            sighup_daemon();
        }
        (Some(_), true) => {
            // Running, cert changed: must restart to pick up new cert
            out.info("TLS cert updated — restarting proxy...");
            run_stop(no_color)?;
            spawn_daemon(&out, false)?;
        }
        (None, _) => {
            // Not running — start it
            out.info("Starting proxy...");
            spawn_daemon(&out, false)?;
        }
    }

    Ok(())
}

/// Internal: runs the actual async proxy server in foreground.
/// Called via `dip proxy serve` by the daemon spawner.
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

// ─── Docker label discovery ───────────────────────────────────────────────────

/// Collect only the domain names for this project's containers (running OR stopped).
/// Used when cleaning up after `dip stop`.
fn discover_project_domains(project: &ProjectConfig) -> Result<std::collections::HashSet<String>> {
    let compose = project.compose_file.to_str().unwrap_or("");
    // -a = include stopped containers; labels are static metadata
    let ps = std::process::Command::new("docker")
        .args(["compose", "-f", compose, "ps", "-q", "-a"])
        .envs(project.get_env())
        .output()?;

    let ids: Vec<String> = String::from_utf8_lossy(&ps.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();

    if ids.is_empty() {
        return Ok(std::collections::HashSet::new());
    }

    let mut inspect_args = vec!["inspect".to_string()];
    inspect_args.extend(ids);
    let inspect = std::process::Command::new("docker")
        .args(&inspect_args)
        .output()?;

    if !inspect.status.success() {
        return Ok(std::collections::HashSet::new());
    }

    let json: serde_json::Value =
        serde_json::from_slice(&inspect.stdout).unwrap_or(serde_json::Value::Array(vec![]));

    let mut domains = std::collections::HashSet::new();
    for container in json.as_array().into_iter().flatten() {
        for (domain, _port) in parse_host_labels(&container["Config"]["Labels"]) {
            domains.insert(domain);
        }
    }

    Ok(domains)
}

/// Read `dip.host: domain:port` labels from running containers and return routes.
/// Container IPs are read from docker network inspect (works on OrbStack / Linux).
fn discover_routes(project: &ProjectConfig) -> Result<Vec<Route>> {
    let compose = project.compose_file.to_str().unwrap_or("");

    // Get IDs of running containers for this compose project
    let ps = std::process::Command::new("docker")
        .args(["compose", "-f", compose, "ps", "-q"])
        .envs(project.get_env())
        .output()?;

    let ids: Vec<String> = String::from_utf8_lossy(&ps.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();

    if ids.is_empty() {
        return Ok(vec![]);
    }

    // Inspect all containers at once
    let mut inspect_args = vec!["inspect".to_string()];
    inspect_args.extend(ids);

    let inspect = std::process::Command::new("docker")
        .args(&inspect_args)
        .output()?;

    if !inspect.status.success() {
        anyhow::bail!(
            "docker inspect failed: {}",
            String::from_utf8_lossy(&inspect.stderr).trim()
        );
    }

    let json: serde_json::Value = serde_json::from_slice(&inspect.stdout)
        .map_err(|e| anyhow::anyhow!("docker inspect JSON parse error: {e}"))?;

    let mut routes = Vec::new();

    for container in json.as_array().into_iter().flatten() {
        let host_entries = parse_host_labels(&container["Config"]["Labels"]);
        if host_entries.is_empty() {
            continue;
        }

        // Get container IP from the first available Docker network
        let ip = container["NetworkSettings"]["Networks"]
            .as_object()
            .and_then(|nets| nets.values().next())
            .and_then(|net| net["IPAddress"].as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        if ip.is_empty() {
            eprintln!("dip-proxy: no IP for container — skipping");
            continue;
        }

        for (domain, port) in host_entries {
            routes.push(Route {
                domain,
                upstream: format!("{ip}:{port}"),
            });
        }
    }

    Ok(routes)
}

// ─── label parsing ────────────────────────────────────────────────────────────

/// Parse all `dip.host*` labels from a container's label map.
///
/// Supports:
///   dip.host: "example.test:80"          → single domain
///   dip.host.web: "example.test:80"      → named alias (same priority as dip.host)
///   dip.host.api: "api.example.test:3000"
///
/// Each value format: "domain:port" or just "domain" (defaults to port 80).
/// Surrounding quotes from YAML double-quoting are stripped automatically.
pub(crate) fn parse_host_labels(labels: &serde_json::Value) -> Vec<(String, String)> {
    let Some(map) = labels.as_object() else {
        return vec![];
    };

    let mut entries = Vec::new();

    for (key, value) in map {
        // Accept "dip.host" (exact) or "dip.host.<anything>"
        if key != "dip.host" && !key.starts_with("dip.host.") {
            continue;
        }

        let raw = value
            .as_str()
            .unwrap_or("")
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim();

        if raw.is_empty() {
            continue;
        }

        let (domain, port) = if let Some((d, p)) = raw.split_once(':') {
            let domain = d.trim().trim_matches('"').trim_matches('\'').to_string();
            let port = p.trim().to_string();
            (domain, port)
        } else {
            (raw.to_string(), "80".to_string())
        };

        if !domain.is_empty() {
            entries.push((domain, port));
        }
    }

    entries
}

// ─── daemon helpers ───────────────────────────────────────────────────────────

fn pid_file() -> PathBuf {
    dirs::proxy_dir().join("proxy.pid")
}
fn log_file() -> PathBuf {
    dirs::proxy_dir().join("proxy.log")
}

fn daemon_pid() -> Option<u32> {
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

fn sighup_daemon() {
    if let Some(pid) = daemon_pid() {
        let _ = std::process::Command::new("kill")
            .args(["-HUP", &pid.to_string()])
            .status();
    }
}

fn prompt_yes(question: &str) -> bool {
    use std::io::{self, BufRead, Write};
    print!("  {question} ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).ok();
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

fn spawn_daemon(out: &Output, verbose: bool) -> Result<()> {
    let log_path = log_file();
    std::fs::create_dir_all(log_path.parent().unwrap())?;

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

// ─── tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> serde_json::Value {
        let map: serde_json::Map<String, serde_json::Value> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), serde_json::Value::String(v.to_string())))
            .collect();
        serde_json::Value::Object(map)
    }

    #[test]
    fn single_host_label() {
        let l = labels(&[("dip.host", "app.test:80")]);
        let entries = parse_host_labels(&l);
        assert_eq!(entries, vec![("app.test".to_string(), "80".to_string())]);
    }

    #[test]
    fn named_host_labels() {
        let l = labels(&[
            ("dip.host.web", "app.test:80"),
            ("dip.host.api", "api.test:3000"),
        ]);
        let mut entries = parse_host_labels(&l);
        entries.sort();
        assert!(entries.contains(&("app.test".to_string(), "80".to_string())));
        assert!(entries.contains(&("api.test".to_string(), "3000".to_string())));
    }

    #[test]
    fn default_port_80_when_omitted() {
        let l = labels(&[("dip.host", "app.test")]);
        let entries = parse_host_labels(&l);
        assert_eq!(entries[0].1, "80");
    }

    #[test]
    fn quoted_values_handled() {
        let l = labels(&[("dip.host", "\"app.test:80\"")]);
        let entries = parse_host_labels(&l);
        assert_eq!(entries[0].0, "app.test");
    }

    #[test]
    fn non_dip_labels_ignored() {
        let l = labels(&[
            ("com.docker.compose.service", "app"),
            ("dip.host", "app.test:80"),
        ]);
        let entries = parse_host_labels(&l);
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn no_labels_returns_empty() {
        let entries = parse_host_labels(&serde_json::Value::Null);
        assert!(entries.is_empty());
    }
}
