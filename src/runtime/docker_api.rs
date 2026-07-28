//! Fast path for read-only `ps` queries: ask the Docker Engine API over its
//! unix socket instead of spawning the docker CLI (which costs ~85ms per
//! call). Powers `dip status` / `health` / `stats` and every other consumer
//! of the `compose ps` patterns.
//!
//! This is an optimization, never a requirement: any failure (no socket,
//! timeout, unexpected response) makes the caller fall back to the CLI.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::UnixStream;

/// Budget for one socket round-trip; miss it and the CLI takes over.
const API_TIMEOUT: Duration = Duration::from_secs(2);

// ─── public entry point ───────────────────────────────────────────────────────

/// Try to answer a `compose ps ...` query via the Engine API.
/// `None` — not an intercepted pattern; `Some(Err)` — caller should fall back.
pub(crate) fn try_ps(project_name: &str, args: &[&str]) -> Option<Result<String>> {
    let query = PsQuery::from_args(args)?;
    let socket = match find_socket() {
        Some(s) => s,
        None => return Some(Err(anyhow::anyhow!("no docker socket found"))),
    };
    Some(run_query(&socket, project_name, &query))
}

// ─── query shape ──────────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum PsOutput {
    /// `ps -a --format json` — one JSON object per line (ContainerRow shape).
    JsonRows,
    /// `ps -q [...]` — one container ID per line.
    Ids,
}

#[derive(Debug, PartialEq)]
struct PsQuery {
    all: bool,
    service: Option<String>,
    output: PsOutput,
}

impl PsQuery {
    fn from_args(args: &[&str]) -> Option<Self> {
        match args {
            ["ps", "-a", "--format", "json"] => Some(PsQuery {
                all: true,
                service: None,
                output: PsOutput::JsonRows,
            }),
            ["ps", "-q", "-a"] => Some(PsQuery {
                all: true,
                service: None,
                output: PsOutput::Ids,
            }),
            ["ps", "-q", "-a", svc] => Some(PsQuery {
                all: true,
                service: Some((*svc).to_string()),
                output: PsOutput::Ids,
            }),
            ["ps", "-q"] => Some(PsQuery {
                all: false,
                service: None,
                output: PsOutput::Ids,
            }),
            ["ps", "-q", svc] => Some(PsQuery {
                all: false,
                service: Some((*svc).to_string()),
                output: PsOutput::Ids,
            }),
            _ => None,
        }
    }

    /// `/containers/json` query string with server-side label filters.
    fn to_path(&self, project_name: &str) -> String {
        // Compose normalizes project names to lowercase for its labels.
        let project = project_name.to_lowercase();
        let mut labels = vec![format!("com.docker.compose.project={project}")];
        if let Some(svc) = &self.service {
            labels.push(format!("com.docker.compose.service={svc}"));
        }
        let mut filters = serde_json::Map::new();
        filters.insert("label".into(), labels.into());
        if !self.all {
            filters.insert("status".into(), vec!["running"].into());
        }
        let filters = Value::Object(filters).to_string();
        format!(
            "/containers/json?all={}&filters={}",
            if self.all { "true" } else { "false" },
            percent_encode(&filters)
        )
    }
}

// ─── socket discovery ─────────────────────────────────────────────────────────

/// First existing socket wins. DOCKER_HOST is authoritative; after that
/// /var/run/docker.sock, which both OrbStack and Docker Desktop maintain
/// while active. Per-app paths cover setups without the system symlink.
/// (Running two engines simultaneously can pick the wrong one — set
/// DOCKER_HOST in that case; the CLI has the same ambiguity.)
fn find_socket() -> Option<PathBuf> {
    socket_candidates(
        std::env::var("DOCKER_HOST").ok().as_deref(),
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    )
    .into_iter()
    .find(|p| p.exists())
}

fn socket_candidates(docker_host: Option<&str>, home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(host) = docker_host
        && let Some(path) = host.strip_prefix("unix://")
    {
        out.push(PathBuf::from(path));
        return out; // explicit override — don't guess further
    }
    out.push(PathBuf::from("/var/run/docker.sock"));
    if let Some(home) = home {
        out.push(home.join(".orbstack/run/docker.sock"));
        out.push(home.join(".docker/run/docker.sock"));
        out.push(home.join(".colima/default/docker.sock"));
    }
    out
}

// ─── request execution ────────────────────────────────────────────────────────

fn run_query(socket: &Path, project_name: &str, query: &PsQuery) -> Result<String> {
    let path = query.to_path(project_name);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    let body = rt.block_on(async {
        tokio::time::timeout(API_TIMEOUT, api_get(socket, &path))
            .await
            .map_err(|_| anyhow::anyhow!("docker API timed out"))?
    })?;

    let containers: Vec<Value> = serde_json::from_str(&body).context("docker API response")?;
    Ok(match query.output {
        PsOutput::JsonRows => render_rows(&containers),
        PsOutput::Ids => render_ids(&containers),
    })
}

async fn api_get(socket: &Path, path: &str) -> Result<String> {
    let stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("connect {}", socket.display()))?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await?;
    tokio::spawn(conn);

    let req = hyper::Request::builder()
        .uri(path)
        .header(hyper::header::HOST, "docker")
        .body(Empty::<Bytes>::new())?;
    let resp = sender.send_request(req).await?;
    if !resp.status().is_success() {
        anyhow::bail!("docker API returned {}", resp.status());
    }
    let body = resp.into_body().collect().await?.to_bytes();
    Ok(String::from_utf8_lossy(&body).into_owned())
}

// ─── response translation ─────────────────────────────────────────────────────

/// Engine `/containers/json` entries → `compose ps --format json` lines
/// (the `ContainerRow` shape: Service/State/Status/Health/Ports).
fn render_rows(containers: &[Value]) -> String {
    let mut rows: Vec<(String, String)> = containers
        .iter()
        .filter_map(|c| {
            let service = c["Labels"]["com.docker.compose.service"].as_str()?;
            let status = c["Status"].as_str().unwrap_or_default();
            let row = serde_json::json!({
                "Service": service,
                "State": c["State"].as_str().unwrap_or_default(),
                "Status": status,
                "Health": health_from_status(status),
                "Ports": format_ports(&c["Ports"]),
            });
            Some((service.to_string(), row.to_string()))
        })
        .collect();
    rows.sort();
    rows.into_iter()
        .map(|(_, line)| line + "\n")
        .collect::<String>()
}

fn render_ids(containers: &[Value]) -> String {
    containers
        .iter()
        .filter(|c| c["Labels"]["com.docker.compose.service"].is_string())
        .filter_map(|c| c["Id"].as_str())
        .map(|id| format!("{id}\n"))
        .collect()
}

/// Compose surfaces health as its own field; the Engine list endpoint only
/// embeds it in the human status string ("Up 2 weeks (healthy)").
fn health_from_status(status: &str) -> &'static str {
    if status.contains("(healthy)") {
        "healthy"
    } else if status.contains("(unhealthy)") {
        "unhealthy"
    } else if status.contains("(health: starting)") {
        "starting"
    } else {
        ""
    }
}

/// Engine port entries → docker-ps style string
/// ("0.0.0.0:8080->80/tcp, 5432/tcp"), deduplicated (v4/v6 pairs).
fn format_ports(ports: &Value) -> String {
    let Some(items) = ports.as_array() else {
        return String::new();
    };
    // Dedup key ignores the IP: the engine reports v4 and v6 as separate
    // entries for the same mapping; showing one keeps the column readable.
    let mut seen: Vec<(u64, Option<u64>, String)> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for p in items {
        let private = p["PrivatePort"].as_u64().unwrap_or(0);
        let proto = p["Type"].as_str().unwrap_or("tcp").to_string();
        let public = p["PublicPort"].as_u64();
        let key = (private, public, proto.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(match public {
            Some(public) => {
                let ip = p["IP"].as_str().unwrap_or("0.0.0.0");
                format!("{ip}:{public}->{private}/{proto}")
            }
            None => format!("{private}/{proto}"),
        });
    }
    out.join(", ")
}

/// Minimal percent-encoding for a query-string value.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len() * 3);
    for b in input.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_all_intercepted_ps_patterns() {
        let q = PsQuery::from_args(&["ps", "-a", "--format", "json"]).unwrap();
        assert_eq!((q.all, q.output), (true, PsOutput::JsonRows));
        let q = PsQuery::from_args(&["ps", "-q"]).unwrap();
        assert_eq!((q.all, q.output), (false, PsOutput::Ids));
        let q = PsQuery::from_args(&["ps", "-q", "-a", "db"]).unwrap();
        assert_eq!(q.service.as_deref(), Some("db"));
        // Anything else stays on the CLI path.
        assert!(PsQuery::from_args(&["up", "-d"]).is_none());
        assert!(PsQuery::from_args(&["ps", "--format", "table"]).is_none());
    }

    #[test]
    fn query_path_filters_by_project_service_and_status() {
        let q = PsQuery::from_args(&["ps", "-q", "db"]).unwrap();
        let path = q.to_path("MyProj");
        assert!(path.starts_with("/containers/json?all=false&filters="));
        let decoded = percent_decode(&path);
        assert!(decoded.contains("com.docker.compose.project=myproj"));
        assert!(decoded.contains("com.docker.compose.service=db"));
        assert!(decoded.contains("running"));
    }

    fn percent_decode(s: &str) -> String {
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap();
                out.push(u8::from_str_radix(hex, 16).unwrap());
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn docker_host_override_stops_candidate_guessing() {
        let c = socket_candidates(Some("unix:///tmp/custom.sock"), Some(Path::new("/Users/x")));
        assert_eq!(c, vec![PathBuf::from("/tmp/custom.sock")]);
        // tcp:// DOCKER_HOST → not usable, fall through to defaults.
        let c = socket_candidates(Some("tcp://1.2.3.4:2375"), Some(Path::new("/Users/x")));
        assert_eq!(c[0], PathBuf::from("/var/run/docker.sock"));
        assert!(c.iter().any(|p| p.ends_with(".orbstack/run/docker.sock")));
    }

    fn engine_fixture() -> Vec<Value> {
        serde_json::from_str(
            r#"[
              {"Id": "aaa111", "State": "running", "Status": "Up 2 weeks (healthy)",
               "Labels": {"com.docker.compose.service": "db",
                          "com.docker.compose.project": "backend"},
               "Ports": [{"IP": "0.0.0.0", "PrivatePort": 3306, "PublicPort": 3307, "Type": "tcp"},
                          {"IP": "::", "PrivatePort": 3306, "PublicPort": 3307, "Type": "tcp"}]},
              {"Id": "bbb222", "State": "exited", "Status": "Exited (1) 3 minutes ago",
               "Labels": {"com.docker.compose.service": "app",
                          "com.docker.compose.project": "backend"},
               "Ports": []},
              {"Id": "ccc333", "State": "running", "Status": "Up 5 days",
               "Labels": {"some.other.label": "not-compose"}, "Ports": []}
            ]"#,
        )
        .unwrap()
    }

    #[test]
    fn renders_compose_ps_rows_sorted_and_filtered() {
        let out = render_rows(&engine_fixture());
        let rows: Vec<Value> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        // Non-compose container dropped; rows sorted by service.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["Service"], "app");
        assert_eq!(rows[0]["State"], "exited");
        assert_eq!(rows[0]["Health"], "");
        assert_eq!(rows[1]["Service"], "db");
        assert_eq!(rows[1]["Health"], "healthy");
        // v4/v6 duplicate collapsed.
        assert_eq!(rows[1]["Ports"], "0.0.0.0:3307->3306/tcp");
    }

    #[test]
    fn renders_ids_one_per_line() {
        let out = render_ids(&engine_fixture());
        assert_eq!(out, "aaa111\nbbb222\n");
    }

    /// Full round-trip against a fake Engine API on a real unix socket.
    #[test]
    fn queries_engine_over_unix_socket() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("docker.sock");
        let sock_for_server = sock_path.clone();

        // Multi-thread runtime: worker threads drive the server task in the
        // background while this test thread blocks in join().
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        // Bind BEFORE spawning the client so the socket is guaranteed to exist.
        let listener = rt
            .block_on(async { tokio::net::UnixListener::bind(&sock_for_server) })
            .unwrap();
        rt.spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(
                            TokioIo::new(stream),
                            hyper::service::service_fn(|req| async move {
                                assert!(req.uri().path().starts_with("/containers/json"));
                                let body = serde_json::to_string(&engine_fixture()).unwrap();
                                Ok::<_, std::convert::Infallible>(hyper::Response::new(
                                    http_body_util::Full::new(Bytes::from(body)),
                                ))
                            }),
                        )
                        .await;
                });
            }
        });

        // run_query builds its own runtime — call it from a plain thread.
        let query = PsQuery::from_args(&["ps", "-a", "--format", "json"]).unwrap();
        let sock = sock_path.clone();
        let out = std::thread::spawn(move || run_query(&sock, "backend", &query))
            .join()
            .unwrap()
            .unwrap();
        assert!(out.contains("\"Service\":\"db\""));
        assert!(out.contains("\"Health\":\"healthy\""));
    }
}
