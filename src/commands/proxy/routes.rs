//! Route management — add, remove, sync, and Docker label discovery.

use anyhow::Result;
use colored::Colorize;

use crate::project::ProjectConfig;
use crate::proxy::certs;
use crate::proxy::config::{self, Route};
use crate::utils::output::Output;

use super::daemon::{daemon_pid, sighup_daemon, spawn_daemon};

// ─── public commands ──────────────────────────────────────────────────────────

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
pub fn run_sync(no_color: bool) -> Result<()> {
    let project = ProjectConfig::load()?;
    sync_from_project(&project, no_color)
}

// ─── lifecycle helpers (called from start.rs / stop.rs) ──────────────────────

/// Best-effort proxy sync called from lifecycle commands (start, restart).
/// Errors are printed as warnings only when verbose — never propagated.
pub fn apply_sync(project: &ProjectConfig, verbose: bool, no_color: bool) {
    if let Err(e) = sync_from_project(project, no_color)
        && verbose
    {
        Output::new(no_color).warning(&format!("Proxy sync: {e}"));
    }
}

/// Best-effort proxy cleanup called from lifecycle commands (stop).
/// Errors are printed as warnings only when verbose — never propagated.
pub fn apply_unsync(project: &ProjectConfig, verbose: bool, no_color: bool) {
    if let Err(e) = unsync_from_project(project, no_color)
        && verbose
    {
        Output::new(no_color).warning(&format!("Proxy cleanup: {e}"));
    }
}

// ─── sync internals ───────────────────────────────────────────────────────────

/// Remove all proxy routes that belong to this project's containers.
pub fn unsync_from_project(project: &ProjectConfig, no_color: bool) -> Result<()> {
    let domains = discover_project_domains(project)?;
    if domains.is_empty() {
        return Ok(());
    }

    let mut cfg = config::load()?;
    let before = cfg.routes.len();
    cfg.routes.retain(|r| !domains.contains(r.domain.as_str()));

    if cfg.routes.len() == before {
        return Ok(());
    }

    config::save(&cfg)?;

    let out = Output::new(no_color);
    for d in &domains {
        out.info(&format!("  proxy: removed {}", d.as_str().dimmed()));
    }

    // Regenerate cert without the removed domains.
    // If the remaining routes no longer need those SANs, the cert shrinks.
    let remaining: Vec<String> = cfg.routes.iter().map(|r| r.domain.clone()).collect();
    let cert_changed = certs::ensure_server_cert(&remaining).unwrap_or(false);

    match (daemon_pid(), cert_changed) {
        (Some(_), false) => sighup_daemon(),
        (Some(_), true) => {
            out.info("TLS cert updated — restarting proxy...");
            super::daemon::run_stop(no_color)?;
            spawn_daemon(&out, false)?;
        }
        (None, _) => {}
    }

    Ok(())
}

/// Discover routes from running containers and update the proxy config.
/// Called from `dip start` after containers are up.
pub fn sync_from_project(project: &ProjectConfig, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);

    let discovered = match discover_routes(project) {
        Ok(r) => r,
        Err(e) => {
            out.warning(&format!("Proxy route discovery failed: {e}"));
            return Ok(());
        }
    };

    if discovered.is_empty() {
        return Ok(());
    }

    let mut cfg = config::load()?;
    let discovered_domains: std::collections::HashSet<&str> =
        discovered.iter().map(|r| r.domain.as_str()).collect();

    cfg.routes
        .retain(|r| !discovered_domains.contains(r.domain.as_str()));
    cfg.routes.extend(discovered.clone());

    // Regenerate cert if new domains appeared
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
        return Ok(());
    }

    match (daemon_pid(), cert_changed) {
        (Some(_), false) => {
            // Running, cert unchanged: hot-reload via SIGHUP
            sighup_daemon();
        }
        (Some(_), true) => {
            // Running, cert changed: restart to pick up new cert
            out.info("TLS cert updated — restarting proxy...");
            super::daemon::run_stop(no_color)?;
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

// ─── Docker label discovery ───────────────────────────────────────────────────

/// Collect domain names for all containers in this project (running or stopped).
fn discover_project_domains(project: &ProjectConfig) -> Result<std::collections::HashSet<String>> {
    let compose = project.compose_file.to_str().unwrap_or("");
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

    let mut args = vec!["inspect".to_string()];
    args.extend(ids);
    let inspect = std::process::Command::new("docker").args(&args).output()?;

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

/// Read `dip.host*` labels from running containers and return proxy routes.
fn discover_routes(project: &ProjectConfig) -> Result<Vec<Route>> {
    let compose = project.compose_file.to_str().unwrap_or("");

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

    let mut args = vec!["inspect".to_string()];
    args.extend(ids);
    let inspect = std::process::Command::new("docker").args(&args).output()?;

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
///   dip.host: "example.test:80"
///   dip.host.web: "example.test:80"
///   dip.host.api: "api.example.test:3000"
///
/// Value format: "domain:port" or "domain" (defaults to port 80).
pub fn parse_host_labels(labels: &serde_json::Value) -> Vec<(String, String)> {
    let Some(map) = labels.as_object() else {
        return vec![];
    };

    let mut entries = Vec::new();

    for (key, value) in map {
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
            (
                d.trim().trim_matches('"').trim_matches('\'').to_string(),
                p.trim().to_string(),
            )
        } else {
            (raw.to_string(), "80".to_string())
        };

        if !domain.is_empty() {
            entries.push((domain, port));
        }
    }

    entries
}

// ─── tests ────────────────────────────────────────────────────────────────────

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
