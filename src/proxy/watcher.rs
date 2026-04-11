/// Docker event watcher.
///
/// Watches `docker events` and keeps proxy routes in sync automatically:
///
///   container start → inspect new IP → add/update route
///   container stop  → remove route
///
/// This handles OrbStack/Docker Desktop restarts where container IPs change —
/// routes update within seconds without any manual `dip start`.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::RwLock;

use super::config::Route;

// ─── types ────────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct DockerEvent {
    #[serde(rename = "Action")]
    action: String,
    id: String,
    #[serde(rename = "Actor")]
    actor: Actor,
}

#[derive(Debug, Deserialize)]
struct Actor {
    #[serde(rename = "Attributes")]
    attributes: HashMap<String, String>,
}

// ─── public entry point ───────────────────────────────────────────────────────

/// Run the Docker event watcher. Restarts automatically on errors.
pub async fn run(routes: Arc<RwLock<Vec<Route>>>) {
    let mut backoff = Duration::from_secs(2);
    loop {
        match watch_loop(&routes).await {
            Ok(()) => {
                // Stream ended cleanly — restart immediately
                backoff = Duration::from_secs(2);
            }
            Err(e) => {
                eprintln!("dip-watcher: {e}, retrying in {}s…", backoff.as_secs());
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
}

// ─── event loop ──────────────────────────────────────────────────────────────

async fn watch_loop(routes: &Arc<RwLock<Vec<Route>>>) -> anyhow::Result<()> {
    let mut child = Command::new("docker")
        .args([
            "events",
            "--format",
            "{{json .}}",
            "--filter",
            "type=container",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| anyhow::anyhow!("cannot spawn `docker events`: {e}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("no stdout from docker events"))?;

    let mut lines = BufReader::new(stdout).lines();

    eprintln!("dip-watcher: watching Docker events");

    while let Some(line) = lines.next_line().await? {
        let Ok(event) = serde_json::from_str::<DockerEvent>(&line) else {
            continue;
        };

        match event.action.as_str() {
            "start" => {
                // Brief pause — container networking takes a moment to init
                tokio::time::sleep(Duration::from_millis(400)).await;
                handle_start(&event, routes).await;
            }
            "stop" | "die" | "kill" | "destroy" => {
                handle_stop(&event, routes).await;
            }
            _ => {}
        }
    }

    anyhow::bail!("docker events stream ended unexpectedly")
}

// ─── event handlers ───────────────────────────────────────────────────────────

/// Container started — inspect its IP and register routes for all dip.host labels.
async fn handle_start(event: &DockerEvent, routes: &Arc<RwLock<Vec<Route>>>) {
    let host_entries = dip_host_entries(&event.actor.attributes);
    if host_entries.is_empty() {
        return; // not a dip container
    }

    let Some(ip) = container_ip(&event.id).await else {
        eprintln!(
            "dip-watcher: cannot get IP for container {}",
            &event.id[..12]
        );
        return;
    };

    let mut w = routes.write().await;
    for (domain, port) in &host_entries {
        let upstream = format!("{ip}:{port}");
        w.retain(|r| r.domain != *domain);
        w.push(Route {
            domain: domain.clone(),
            upstream: upstream.clone(),
        });
        eprintln!("dip-watcher: + {domain} → {upstream}");
    }
}

/// Container stopped — remove its routes.
async fn handle_stop(event: &DockerEvent, routes: &Arc<RwLock<Vec<Route>>>) {
    let domains: Vec<String> = dip_host_entries(&event.actor.attributes)
        .into_iter()
        .map(|(domain, _)| domain)
        .collect();

    if domains.is_empty() {
        return;
    }

    let mut w = routes.write().await;
    let before = w.len();
    w.retain(|r| !domains.contains(&r.domain));
    if w.len() < before {
        for d in &domains {
            eprintln!("dip-watcher: - {d}");
        }
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Extract all `dip.host` / `dip.host.*` label values from a container's attributes.
/// Returns `(domain, port)` pairs.
fn dip_host_entries(attrs: &HashMap<String, String>) -> Vec<(String, String)> {
    attrs
        .iter()
        .filter(|(k, _)| k.as_str() == "dip.host" || k.starts_with("dip.host."))
        .map(|(_, v)| parse_host_value(v))
        .collect()
}

/// Get the primary IP address of a running container.
async fn container_ip(id: &str) -> Option<String> {
    let out = Command::new("docker")
        .args([
            "inspect",
            "--format",
            "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
            id,
        ])
        .output()
        .await
        .ok()?;

    let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if ip.is_empty() { None } else { Some(ip) }
}

/// Parse `"domain:port"` or `"domain"` → `(domain, port)`.
fn parse_host_value(value: &str) -> (String, String) {
    let value = value.trim().trim_matches('"').trim_matches('\'');
    if let Some((d, p)) = value.split_once(':') {
        (d.trim().to_string(), p.trim().to_string())
    } else {
        (value.to_string(), "80".to_string())
    }
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_host_with_port() {
        let (d, p) = parse_host_value("backend.test:3000");
        assert_eq!(d, "backend.test");
        assert_eq!(p, "3000");
    }

    #[test]
    fn parse_host_without_port_defaults_80() {
        let (d, p) = parse_host_value("backend.test");
        assert_eq!(d, "backend.test");
        assert_eq!(p, "80");
    }

    #[test]
    fn parse_host_strips_quotes() {
        let (d, p) = parse_host_value("\"backend.test:80\"");
        assert_eq!(d, "backend.test");
        assert_eq!(p, "80");
    }

    #[test]
    fn dip_host_entries_picks_all_labels() {
        let mut attrs = HashMap::new();
        attrs.insert("dip.host".to_string(), "app.test:80".to_string());
        attrs.insert("dip.host.api".to_string(), "api.test:3000".to_string());
        attrs.insert(
            "com.docker.compose.project".to_string(),
            "myapp".to_string(),
        );

        let mut entries = dip_host_entries(&attrs);
        entries.sort();
        assert_eq!(entries.len(), 2);
        assert!(entries.contains(&("api.test".to_string(), "3000".to_string())));
        assert!(entries.contains(&("app.test".to_string(), "80".to_string())));
    }

    #[test]
    fn dip_host_entries_empty_for_non_dip_container() {
        let mut attrs = HashMap::new();
        attrs.insert(
            "com.docker.compose.service".to_string(),
            "redis".to_string(),
        );
        assert!(dip_host_entries(&attrs).is_empty());
    }
}
