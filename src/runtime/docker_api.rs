//! Fast path for read-only queries: ask the Docker Engine API directly
//! instead of spawning the docker CLI. Powers `dip status` / `health` /
//! `stats` and every other consumer of the `compose ps` patterns, and
//! resolves service names to container IDs so `exec` can skip the compose
//! plugin.
//!
//! Measured on macOS: `docker --version` 9 ms, `docker compose version`
//! 36 ms, `docker compose -f file config` 45 ms — the plugin costs ~35 ms of
//! process boot before it looks at anything. One API round-trip is ~1 ms.
//!
//! This is an optimization, never a requirement: any failure (no reachable
//! endpoint, timeout, unexpected response) makes the caller fall back to the
//! CLI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};

/// Budget for one round-trip; miss it and the CLI takes over.
const API_TIMEOUT: Duration = Duration::from_secs(2);

/// Separate, much tighter budget for reaching the daemon at all. A unix socket
/// connects instantly and a VM on the host in about a millisecond, so anything
/// slower is a daemon that is down or unreachable — and the whole point of this
/// path is to be faster than the CLI, not to sit in a connect() that a
/// stopped-daemon TCP endpoint would hold for the full request budget.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);

// ─── public entry point ───────────────────────────────────────────────────────

/// Try to answer a `compose ps ...` query via the Engine API.
/// `None` — not an intercepted pattern; `Some(Err)` — caller should fall back.
pub(crate) fn try_ps(project_name: &str, args: &[&str]) -> Option<Result<String>> {
    let query = PsQuery::from_args(args)?;
    let endpoint = match find_endpoint() {
        Some(e) => e,
        None => return Some(Err(anyhow::anyhow!("no reachable docker endpoint"))),
    };
    Some(run_query(&endpoint, project_name, &query))
}

/// Resolve a compose service to the ID of its single running container.
///
/// Errs when the answer is anything but exactly one container, so the caller
/// can hand the case back to compose: zero means "not started" (compose owns
/// that error message) and more than one means the service is scaled, where
/// picking a replica is `compose exec --index`'s job, not ours.
///
/// Takes the endpoint rather than resolving one, so a caller that goes on to
/// use it — `exec` resolves the container and then runs in it — pays for the
/// context lookup once.
pub(super) fn container_id_at(
    endpoint: &Endpoint,
    project_name: &str,
    service: &str,
) -> Result<String> {
    let query = PsQuery {
        all: false,
        service: Some(service.to_string()),
        output: PsOutput::Ids,
    };
    let containers = get_containers(endpoint, &query.to_path(project_name))?;
    match containers.as_slice() {
        [one] => one["Id"]
            .as_str()
            .map(str::to_string)
            .context("container entry without an Id"),
        [] => anyhow::bail!("no running container for service '{service}'"),
        many => anyhow::bail!("service '{service}' has {} containers", many.len()),
    }
}

/// Whether the daemon answers at all.
///
/// `/_ping` is the cheapest endpoint it serves — ~0.4 ms against ~45 ms for
/// `docker info`, which is what this replaces. A `false` here is not proof the
/// daemon is down (the endpoint may just not be one we can speak to), so
/// callers fall back to the CLI rather than reporting an outage.
pub(crate) fn ping() -> bool {
    let Some(endpoint) = find_endpoint() else {
        return false;
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return false,
    };
    rt.block_on(async {
        tokio::time::timeout(API_TIMEOUT, api_get(&endpoint, "/_ping"))
            .await
            .is_ok_and(|r| r.is_ok())
    })
}

/// Running container counts per compose project, from a single query.
///
/// `dip ls` used to spawn one `docker ps` per discovered project (~24 ms
/// each); the daemon can list every running container in one round trip and
/// the tallying is ours to do. Keys are compose's normalized project names,
/// so look up with [`normalize_project_name`].
pub(crate) fn running_counts_by_project() -> Result<HashMap<String, usize>> {
    let endpoint = find_endpoint().context("no reachable docker endpoint")?;
    let filters = serde_json::json!({ "status": ["running"] }).to_string();
    let path = format!(
        "/containers/json?all=false&filters={}",
        percent_encode(&filters)
    );
    Ok(tally_projects(&get_containers(&endpoint, &path)?))
}

fn tally_projects(containers: &[Value]) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for c in containers {
        if let Some(project) = c["Labels"]["com.docker.compose.project"].as_str() {
            *counts.entry(project.to_string()).or_insert(0) += 1;
        }
    }
    counts
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
        let project = normalize_project_name(project_name);
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

/// Reproduce compose's own project-name normalization, which is what its
/// container labels actually carry.
///
/// `dip init` defaults the project name to the directory name, so a directory
/// like `My.App` or `client site` reaches us with characters compose strips.
/// Lowercasing alone would query a label that no container has, and the label
/// filter is applied server-side — the daemon would answer "no containers"
/// rather than error, so `dip status` would render an empty table instead of
/// falling back to the CLI.
///
/// Compose: lowercase, keep only `[a-z0-9_-]`, then trim leading `_-`.
pub(crate) fn normalize_project_name(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
        .collect::<String>()
        .trim_start_matches(['_', '-'])
        .to_string()
}

// ─── endpoint discovery ───────────────────────────────────────────────────────

/// Where the Engine API lives for the daemon the docker CLI would talk to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Endpoint {
    Unix(PathBuf),
    /// Plain HTTP over TCP ("host:port") — `tcp://` and `http://` endpoints.
    Tcp(String),
}

/// Resolve the endpoint the same way the docker CLI does, in its order of
/// precedence: `DOCKER_HOST`, then `DOCKER_CONTEXT`, then the `currentContext`
/// recorded in ~/.docker/config.json, then the built-in default.
///
/// Honouring contexts is not optional politeness. `/var/run/docker.sock` is
/// left behind by whichever engine was installed last, so a machine with an
/// active non-default context (OrbStack, Colima, a remote host) can have both
/// a live context endpoint *and* a stale socket belonging to a different
/// daemon. Guessing the socket would answer from the wrong engine — container
/// IDs that the CLI then cannot find.
pub(super) fn find_endpoint() -> Option<Endpoint> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    resolve_endpoint(
        std::env::var("DOCKER_HOST").ok().as_deref(),
        std::env::var("DOCKER_CONTEXT").ok().as_deref(),
        home.as_deref(),
    )
}

fn resolve_endpoint(
    docker_host: Option<&str>,
    docker_context: Option<&str>,
    home: Option<&Path>,
) -> Option<Endpoint> {
    if let Some(host) = docker_host.filter(|h| !h.is_empty()) {
        // Explicit override — parse it or give up; never fall through to a
        // guess that would point at a different daemon.
        return parse_endpoint(host).filter(is_reachable);
    }

    let context = docker_context
        .filter(|c| !c.is_empty())
        .map(str::to_string)
        .or_else(|| current_context(home?))
        .unwrap_or_else(|| "default".to_string());

    if context == "default" {
        return default_socket_candidates(home)
            .into_iter()
            .find(|p| p.exists())
            .map(Endpoint::Unix);
    }

    parse_endpoint(&context_host(home?, &context)?).filter(is_reachable)
}

/// `tcp://` and `http://` are the daemon's plaintext HTTP endpoints. `https://`
/// and `ssh://` need TLS/an ssh tunnel; both fall back to the CLI.
fn parse_endpoint(host: &str) -> Option<Endpoint> {
    if let Some(path) = host.strip_prefix("unix://") {
        return Some(Endpoint::Unix(PathBuf::from(path)));
    }
    for scheme in ["tcp://", "http://"] {
        if let Some(rest) = host.strip_prefix(scheme) {
            let authority = rest.trim_end_matches('/');
            if authority.is_empty() {
                return None;
            }
            // Default to the daemon's unencrypted port when none is given.
            return Some(Endpoint::Tcp(if authority.contains(':') {
                authority.to_string()
            } else {
                format!("{authority}:2375")
            }));
        }
    }
    None
}

/// A unix path that isn't there can't be the live daemon; TCP is checked by
/// connecting, so it always passes here.
fn is_reachable(endpoint: &Endpoint) -> bool {
    match endpoint {
        Endpoint::Unix(path) => path.exists(),
        Endpoint::Tcp(_) => true,
    }
}

impl Endpoint {
    /// How the endpoint reads in a verbose log or a fall-back reason.
    fn describe(&self) -> String {
        match self {
            Endpoint::Unix(path) => path.display().to_string(),
            Endpoint::Tcp(authority) => authority.clone(),
        }
    }
}

/// Sockets the default context may be served from: `/var/run/docker.sock`,
/// which both OrbStack and Docker Desktop maintain while active, then the
/// per-app paths that cover setups without the system symlink.
fn default_socket_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("/var/run/docker.sock")];
    if let Some(home) = home {
        out.push(home.join(".orbstack/run/docker.sock"));
        out.push(home.join(".docker/run/docker.sock"));
        out.push(home.join(".colima/default/docker.sock"));
    }
    out
}

fn current_context(home: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(home.join(".docker/config.json")).ok()?;
    let config: Value = serde_json::from_str(&raw).ok()?;
    config["currentContext"].as_str().map(str::to_string)
}

/// The docker endpoint of a named context.
///
/// The CLI stores each context under a directory named after the SHA-256 of
/// its name; rather than hashing, scan the handful of metadata files and match
/// on the `Name` they carry.
fn context_host(home: &Path, context: &str) -> Option<String> {
    let meta_dir = home.join(".docker/contexts/meta");
    for entry in std::fs::read_dir(meta_dir).ok()? {
        let Ok(entry) = entry else { continue };
        let Ok(raw) = std::fs::read_to_string(entry.path().join("meta.json")) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if meta["Name"].as_str() == Some(context) {
            return meta["Endpoints"]["docker"]["Host"]
                .as_str()
                .map(str::to_string);
        }
    }
    None
}

// ─── request execution ────────────────────────────────────────────────────────

fn run_query(endpoint: &Endpoint, project_name: &str, query: &PsQuery) -> Result<String> {
    let containers = get_containers(endpoint, &query.to_path(project_name))?;
    Ok(match query.output {
        PsOutput::JsonRows => render_rows(&containers),
        PsOutput::Ids => render_ids(&containers),
    })
}

fn get_containers(endpoint: &Endpoint, path: &str) -> Result<Vec<Value>> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?;
    let body = rt.block_on(async {
        tokio::time::timeout(API_TIMEOUT, api_get(endpoint, path))
            .await
            .map_err(|_| anyhow::anyhow!("docker API timed out"))?
    })?;
    serde_json::from_str(&body).context("docker API response")
}

/// Erases the stream type so unix and TCP endpoints share one HTTP path.
pub(super) trait ApiStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> ApiStream for T {}

pub(super) async fn connect(endpoint: &Endpoint) -> Result<Pin<Box<dyn ApiStream>>> {
    let target = endpoint.describe();
    let connected: std::io::Result<Pin<Box<dyn ApiStream>>> =
        tokio::time::timeout(CONNECT_TIMEOUT, async {
            match endpoint {
                Endpoint::Unix(path) => Ok(Box::pin(UnixStream::connect(path).await?) as _),
                Endpoint::Tcp(authority) => {
                    let stream = TcpStream::connect(authority).await?;
                    // Single small request/response: Nagle would only add latency.
                    let _ = stream.set_nodelay(true);
                    Ok(Box::pin(stream) as _)
                }
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("connect {target} timed out"))?;
    connected.with_context(|| format!("connect {target}"))
}

async fn api_get(endpoint: &Endpoint, path: &str) -> Result<String> {
    let io = TokioIo::new(connect(endpoint).await?);
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
    fn project_names_are_normalized_the_way_compose_labels_them() {
        assert_eq!(normalize_project_name("myapp"), "myapp");
        assert_eq!(normalize_project_name("MyApp"), "myapp");
        // Characters outside [a-z0-9_-] are dropped, not replaced — a dotted
        // or spaced directory name still has to match compose's label.
        assert_eq!(normalize_project_name("My.App"), "myapp");
        assert_eq!(normalize_project_name("client site"), "clientsite");
        assert_eq!(normalize_project_name("my-app_2"), "my-app_2");
        // Leading separators are trimmed.
        assert_eq!(normalize_project_name("_-app"), "app");
        // Non-ASCII is dropped too (to_lowercase would otherwise keep it).
        assert_eq!(normalize_project_name("café"), "caf");
    }

    #[test]
    fn parses_the_endpoint_schemes_the_daemon_serves_in_plaintext() {
        assert_eq!(
            parse_endpoint("unix:///tmp/custom.sock"),
            Some(Endpoint::Unix(PathBuf::from("/tmp/custom.sock")))
        );
        assert_eq!(
            parse_endpoint("tcp://192.168.127.2:2375"),
            Some(Endpoint::Tcp("192.168.127.2:2375".into()))
        );
        assert_eq!(
            parse_endpoint("http://127.0.0.1:2375/"),
            Some(Endpoint::Tcp("127.0.0.1:2375".into()))
        );
        // No port given — the daemon's unencrypted default.
        assert_eq!(
            parse_endpoint("tcp://dockerhost"),
            Some(Endpoint::Tcp("dockerhost:2375".into()))
        );
        // TLS and ssh need machinery we don't have here — fall back to the CLI.
        assert_eq!(parse_endpoint("https://1.2.3.4:2376"), None);
        assert_eq!(parse_endpoint("ssh://user@host"), None);
        assert_eq!(parse_endpoint("tcp://"), None);
    }

    /// Lays out a fake ~/.docker: a current context plus one context whose
    /// endpoint is a TCP daemon, mirroring a `docker context use` setup.
    fn docker_home(current: Option<&str>) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let dot = home.path().join(".docker");
        std::fs::create_dir_all(dot.join("contexts/meta/deadbeef")).unwrap();
        std::fs::write(
            dot.join("contexts/meta/deadbeef/meta.json"),
            r#"{"Name":"remote","Endpoints":{"docker":{"Host":"tcp://192.168.127.2:2375"}}}"#,
        )
        .unwrap();
        if let Some(current) = current {
            std::fs::write(
                dot.join("config.json"),
                format!(r#"{{"currentContext":"{current}"}}"#),
            )
            .unwrap();
        }
        home
    }

    #[test]
    fn active_context_wins_over_a_stale_default_socket() {
        let home = docker_home(Some("remote"));
        // /var/run/docker.sock exists on most dev machines and belongs to
        // whichever engine was installed last — the context must win, or we
        // would answer from a different daemon than the CLI talks to.
        assert_eq!(
            resolve_endpoint(None, None, Some(home.path())),
            Some(Endpoint::Tcp("192.168.127.2:2375".into()))
        );
    }

    #[test]
    fn docker_host_outranks_the_context() {
        let home = docker_home(Some("remote"));
        assert_eq!(
            resolve_endpoint(Some("tcp://10.0.0.1:2375"), None, Some(home.path())),
            Some(Endpoint::Tcp("10.0.0.1:2375".into()))
        );
        // DOCKER_CONTEXT sits between the two.
        assert_eq!(
            resolve_endpoint(None, Some("remote"), Some(home.path())),
            Some(Endpoint::Tcp("192.168.127.2:2375".into()))
        );
    }

    #[test]
    fn unusable_overrides_never_fall_through_to_a_guess() {
        let home = docker_home(Some("remote"));
        // An https:// DOCKER_HOST is a real daemon we can't reach here.
        // Guessing a local socket instead would silently query the wrong one.
        assert_eq!(
            resolve_endpoint(Some("https://1.2.3.4:2376"), None, Some(home.path())),
            None
        );
        // Same for a DOCKER_HOST unix path that doesn't exist.
        assert_eq!(
            resolve_endpoint(Some("unix:///nope/docker.sock"), None, Some(home.path())),
            None
        );
        // And for a context name with no metadata on disk.
        assert_eq!(
            resolve_endpoint(None, Some("missing"), Some(home.path())),
            None
        );
    }

    #[test]
    fn default_context_searches_the_well_known_sockets() {
        let home = docker_home(Some("default"));
        let candidates = default_socket_candidates(Some(home.path()));
        assert_eq!(candidates[0], PathBuf::from("/var/run/docker.sock"));
        assert!(
            candidates
                .iter()
                .any(|p| p.ends_with(".orbstack/run/docker.sock"))
        );
        assert!(
            candidates
                .iter()
                .any(|p| p.ends_with(".colima/default/docker.sock"))
        );
        // No config.json at all behaves like the default context.
        let bare = docker_home(None);
        let resolved = resolve_endpoint(None, None, Some(bare.path()));
        assert!(resolved.is_none() || matches!(resolved, Some(Endpoint::Unix(_))));
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
    fn tallies_running_containers_per_compose_project() {
        let containers: Vec<Value> = serde_json::from_str(
            r#"[
              {"Labels": {"com.docker.compose.project": "backend"}},
              {"Labels": {"com.docker.compose.project": "backend"}},
              {"Labels": {"com.docker.compose.project": "shop"}},
              {"Labels": {"some.other.label": "not-compose"}},
              {"Labels": {}}
            ]"#,
        )
        .unwrap();
        let counts = tally_projects(&containers);
        assert_eq!(counts.get("backend"), Some(&2));
        assert_eq!(counts.get("shop"), Some(&1));
        // Containers outside compose are not attributed to any project.
        assert_eq!(counts.len(), 2);
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
        let endpoint = Endpoint::Unix(sock_path.clone());
        let out = std::thread::spawn(move || run_query(&endpoint, "backend", &query))
            .join()
            .unwrap()
            .unwrap();
        assert!(out.contains("\"Service\":\"db\""));
        assert!(out.contains("\"Health\":\"healthy\""));
    }

    /// Serves a canned container list on a unix socket and returns its path.
    /// The returned TempDir must be kept alive for the socket to survive.
    fn fake_engine(containers: Vec<Value>) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("docker.sock");
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let listener = rt
            .block_on({
                let p = sock_path.clone();
                async move { tokio::net::UnixListener::bind(&p) }
            })
            .unwrap();
        let body = serde_json::to_string(&containers).unwrap();
        rt.spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let body = body.clone();
                tokio::spawn(async move {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(
                            TokioIo::new(stream),
                            hyper::service::service_fn(move |_req| {
                                let body = body.clone();
                                async move {
                                    Ok::<_, std::convert::Infallible>(hyper::Response::new(
                                        http_body_util::Full::new(Bytes::from(body)),
                                    ))
                                }
                            }),
                        )
                        .await;
                });
            }
        });
        // Leak the runtime so its worker keeps serving for the test's lifetime.
        std::mem::forget(rt);
        (dir, sock_path)
    }

    fn resolve_id(containers: Vec<Value>) -> Result<String> {
        let (_dir, sock) = fake_engine(containers);
        let endpoint = Endpoint::Unix(sock);
        // container_id_at builds its own runtime — call it from a plain thread.
        std::thread::spawn(move || container_id_at(&endpoint, "backend", "db"))
            .join()
            .unwrap()
    }

    #[test]
    fn resolves_a_single_running_container_to_its_id() {
        let one = serde_json::from_str::<Vec<Value>>(
            r#"[{"Id":"aaa111","Labels":{"com.docker.compose.service":"db"}}]"#,
        )
        .unwrap();
        assert_eq!(resolve_id(one).unwrap(), "aaa111");
    }

    #[test]
    fn refuses_to_guess_when_the_service_is_stopped_or_scaled() {
        // Nothing running: compose owns the "is it started?" error.
        let err = resolve_id(vec![]).unwrap_err().to_string();
        assert!(err.contains("no running container"), "{err}");

        // Scaled: picking a replica is `compose exec --index`'s job.
        let two = serde_json::from_str::<Vec<Value>>(
            r#"[{"Id":"aaa111","Labels":{}},{"Id":"bbb222","Labels":{}}]"#,
        )
        .unwrap();
        let err = resolve_id(two).unwrap_err().to_string();
        assert!(err.contains("has 2 containers"), "{err}");
    }
}
