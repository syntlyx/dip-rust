//! Upstream dialing strategy: direct TCP, or SOCKS5 through whalet's agent.
//!
//! On Linux the host reaches container bridge networks directly, so the proxy
//! just connects. On macOS container IPs live inside whalet's Linux VM and
//! are unreachable from the host; whalet forwards a SOCKS5 agent out of the
//! VM's root netns, which gives rootless access to any container IP.
//!
//! The agent port is dynamic: whalet takes 1080 if free, otherwise any open
//! port, and publishes the real address in `whalet status`. We never dial a
//! conventional port blind — if some other SOCKS proxy (ssh -D, shadowsocks,
//! a container publishing 1080) squats there, it would silently receive
//! container-bound traffic. whalet's own status output is the only source
//! of truth, and the address it names is still handshake-probed before use.
//!
//! Detection is live, re-checked every RECHECK, so the proxy follows the app
//! being started or quit mid-session. Pooled keep-alive connections opened
//! before a flip keep their old path until they idle out; only new connects
//! see the new mode.
//!
//! `DIP_SOCKS` overrides detection:
//!   DIP_SOCKS=off          never use SOCKS (also `0`, `false`, `no`)
//!   DIP_SOCKS=host:port    probe this address instead of asking whalet

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::RwLock;
use std::task::{Context, Poll};
use std::time::Duration;

use hyper::Uri;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_socks::tcp::Socks5Stream;

use super::BoxError;

/// How often to re-check whether the agent is alive. When the last known
/// address still answers, a check is one local TCP handshake plus 5 bytes;
/// `whalet status` is only consulted when that fails.
const RECHECK: Duration = Duration::from_secs(10);

/// Budget for one liveness probe (TCP connect + SOCKS5 greeting).
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);

/// Budget for asking `whalet status` where the agent listens.
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(2);

/// Agent address the last check saw alive; `None` means dial direct.
/// Written only by the refresh task, read per-connect.
static AGENT: RwLock<Option<SocketAddr>> = RwLock::new(None);

/// Where refresh() gets its candidate address from — decided once at startup.
#[derive(Clone, Copy)]
enum Source {
    /// DIP_SOCKS=host:port — trust the user, skip whalet discovery.
    Forced(SocketAddr),
    /// Ask `whalet status` for the published agent address.
    Whalet,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialer {
    Direct,
    Socks5(SocketAddr),
}

impl Dialer {
    /// Open a TCP connection to `target` ("host:port"). No timeout here —
    /// callers own the deadline (upstream.rs and DipConnector both wrap this).
    pub async fn connect(self, target: &str) -> Result<TcpStream, BoxError> {
        match self {
            Dialer::Direct => Ok(TcpStream::connect(target).await?),
            Dialer::Socks5(agent) => {
                let stream = Socks5Stream::connect(agent, target)
                    .await
                    .map_err(|e| format!("via SOCKS agent {agent}: {e}"))?;
                Ok(stream.into_inner())
            }
        }
    }
}

/// The dialer to use for the next connect, per the latest check.
pub fn current() -> Dialer {
    match *AGENT.read().unwrap() {
        Some(addr) => Dialer::Socks5(addr),
        None => Dialer::Direct,
    }
}

/// Decide the detection source, run the first check (so the startup log line
/// is accurate), and spawn the background recheck task.
pub async fn init() {
    let source = match std::env::var("DIP_SOCKS") {
        Ok(v)
            if matches!(
                v.to_ascii_lowercase().as_str(),
                "off" | "0" | "false" | "no"
            ) =>
        {
            return;
        }
        Ok(v) => match v.parse::<SocketAddr>() {
            Ok(addr) => Source::Forced(addr),
            Err(_) => {
                eprintln!("dip-proxy: ignoring DIP_SOCKS={v}: expected host:port or `off`");
                return;
            }
        },
        // Only macOS needs the agent — on Linux direct connects reach
        // container networks and whalet doesn't exist.
        Err(_) if cfg!(target_os = "macos") => Source::Whalet,
        Err(_) => return,
    };

    refresh(source).await;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(RECHECK).await;
            refresh(source).await;
        }
    });
}

/// One detection pass: find the agent, verify it answers SOCKS5, and update
/// the global state, logging transitions in either direction.
async fn refresh(source: Source) {
    let known = *AGENT.read().unwrap();

    let live = match source {
        Source::Forced(addr) => probe(addr).await.then_some(addr),
        Source::Whalet => match known {
            // Last known address still answers → no subprocess needed.
            Some(addr) if probe(addr).await => Some(addr),
            // Unknown, or the agent moved (whalet restart can land on a new
            // port when 1080 was taken) → ask whalet where it listens now.
            _ => match discover().await {
                Some(addr) if probe(addr).await => Some(addr),
                _ => None,
            },
        },
    };

    if live != known {
        match live {
            Some(addr) => eprintln!(
                "dip-proxy: whalet SOCKS agent live at {addr} — container connects go through it"
            ),
            None => eprintln!("dip-proxy: whalet SOCKS agent gone — using direct connects"),
        }
        *AGENT.write().unwrap() = live;
    }
}

/// Ask `whalet status` for the published agent address.
async fn discover() -> Option<SocketAddr> {
    let output = tokio::time::timeout(
        DISCOVER_TIMEOUT,
        tokio::process::Command::new("whalet")
            .arg("status")
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_agent_addr(&String::from_utf8_lossy(&output.stdout))
}

/// Extract the address from the status line, e.g.
/// `socks5 proxy: 127.0.0.1:1080 (reaches container addresses)` —
/// only the first token after the key is the address.
fn parse_agent_addr(status: &str) -> Option<SocketAddr> {
    const KEY: &str = "socks5 proxy:";
    status.lines().find_map(|line| {
        let pos = line.to_ascii_lowercase().find(KEY)?;
        line[pos + KEY.len()..]
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    })
}

/// True if `addr` answers a SOCKS5 no-auth greeting. Guards against stale
/// status info and non-SOCKS listeners: whalet may have died since it
/// published the address, and whatever answers must at least speak SOCKS5.
async fn probe(addr: SocketAddr) -> bool {
    let handshake = async {
        let mut stream = TcpStream::connect(addr).await.ok()?;
        // VER=5, NMETHODS=1, METHODS=[NO AUTH]
        stream.write_all(&[0x05, 0x01, 0x00]).await.ok()?;
        let mut reply = [0u8; 2];
        stream.read_exact(&mut reply).await.ok()?;
        (reply == [0x05, 0x00]).then_some(())
    };
    tokio::time::timeout(PROBE_TIMEOUT, handshake)
        .await
        .ok()
        .flatten()
        .is_some()
}

/// Connector for the pooled hyper client: same dial path as the streaming
/// code, so SOCKS mode covers pooled requests too (HttpConnector would
/// silently keep dialing direct).
#[derive(Clone)]
pub struct DipConnector {
    connect_timeout: Duration,
}

impl DipConnector {
    pub fn new(connect_timeout: Duration) -> Self {
        Self { connect_timeout }
    }
}

impl tower_service::Service<Uri> for DipConnector {
    type Response = TokioIo<TcpStream>;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let timeout = self.connect_timeout;
        Box::pin(async move {
            let host = uri.host().ok_or("upstream URI missing host")?;
            let target = format!("{host}:{}", uri.port_u16().unwrap_or(80));
            match tokio::time::timeout(timeout, current().connect(&target)).await {
                Ok(Ok(stream)) => Ok(TokioIo::new(stream)),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(format!(
                    "connect to {target} timed out after {}s (container gone?)",
                    timeout.as_secs()
                )
                .into()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn parses_agent_addr_from_status_output() {
        // Real `whalet status` output — note the trailing annotation.
        let status = "whalet: running (pid 44846), docker 29.7.1 ready\n\
                      docker socket: /Users/x/.local/state/whalet/run/docker.sock\n\
                      socks5 proxy: 127.0.0.1:1080 (reaches container addresses)\n";
        assert_eq!(
            parse_agent_addr(status),
            Some("127.0.0.1:1080".parse().unwrap())
        );
        // Fallback port, no annotation.
        assert_eq!(
            parse_agent_addr("socks5 proxy: 127.0.0.1:52341"),
            Some("127.0.0.1:52341".parse().unwrap())
        );
    }

    #[test]
    fn status_without_socks_line_yields_none() {
        // Older whalet builds don't publish the agent line at all.
        let status = "whalet: running (pid 41817), docker 29.7.1 ready\n\
                      docker socket: /Users/x/.local/state/whalet/run/docker.sock\n";
        assert_eq!(parse_agent_addr(status), None);
    }

    #[test]
    fn malformed_socks_line_yields_none() {
        assert_eq!(parse_agent_addr("socks5 proxy: starting...\n"), None);
        assert_eq!(parse_agent_addr("socks5 proxy:\n"), None);
    }

    /// Minimal RFC 1928 server: no-auth handshake, CONNECT, then a blind
    /// relay to the requested target. Enough to exercise the real
    /// tokio-socks client end-to-end.
    async fn spawn_fake_socks() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut client, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let mut greeting = [0u8; 3];
                    client.read_exact(&mut greeting).await.ok()?;
                    client.write_all(&[0x05, 0x00]).await.ok()?;

                    let mut head = [0u8; 4];
                    client.read_exact(&mut head).await.ok()?;
                    let target = match head[3] {
                        // ATYP=1: IPv4
                        0x01 => {
                            let mut rest = [0u8; 6];
                            client.read_exact(&mut rest).await.ok()?;
                            let ip = std::net::Ipv4Addr::new(rest[0], rest[1], rest[2], rest[3]);
                            let port = u16::from_be_bytes([rest[4], rest[5]]);
                            format!("{ip}:{port}")
                        }
                        // ATYP=3: domain
                        0x03 => {
                            let mut len = [0u8; 1];
                            client.read_exact(&mut len).await.ok()?;
                            let mut buf = vec![0u8; len[0] as usize + 2];
                            client.read_exact(&mut buf).await.ok()?;
                            let port = u16::from_be_bytes([
                                buf[len[0] as usize],
                                buf[len[0] as usize + 1],
                            ]);
                            let host = String::from_utf8_lossy(&buf[..len[0] as usize]).to_string();
                            format!("{host}:{port}")
                        }
                        _ => return None,
                    };

                    let mut upstream = TcpStream::connect(&target).await.ok()?;
                    client
                        .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
                        .await
                        .ok()?;
                    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                    Some(())
                });
            }
        });
        addr
    }

    #[tokio::test]
    async fn probe_accepts_socks5_server() {
        let agent = spawn_fake_socks().await;
        assert!(probe(agent).await);
    }

    #[tokio::test]
    async fn probe_rejects_non_socks_listener() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                // An HTTP server squatting on the port: answers, but not SOCKS.
                let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
            }
        });
        assert!(!probe(addr).await);
    }

    #[tokio::test]
    async fn probe_rejects_dead_port() {
        // Bind then drop: the port existed a moment ago but nobody listens now.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(!probe(addr).await);
    }

    #[tokio::test]
    async fn socks5_dialer_relays_to_target() {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = echo.accept().await.unwrap();
            let mut buf = [0u8; 5];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(&buf).await.unwrap();
        });

        let agent = spawn_fake_socks().await;
        let mut stream = Dialer::Socks5(agent)
            .connect(&echo_addr.to_string())
            .await
            .unwrap();

        stream.write_all(b"hello").await.unwrap();
        let mut reply = [0u8; 5];
        stream.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"hello");
    }

    #[tokio::test]
    async fn direct_dialer_still_connects() {
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = echo.accept().await.unwrap();
            let mut buf = [0u8; 2];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(&buf).await.unwrap();
        });

        let mut stream = Dialer::Direct
            .connect(&echo_addr.to_string())
            .await
            .unwrap();
        stream.write_all(b"ok").await.unwrap();
        let mut reply = [0u8; 2];
        stream.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"ok");
    }
}
