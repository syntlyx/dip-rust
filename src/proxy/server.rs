use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1 as client_http1;
use hyper::server::conn::http1 as server_http1;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto as server_auto;
use rustls::ServerConfig;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;
use tokio_rustls::TlsAcceptor;

use super::config::{ProxyConfig, Route};
use super::router;

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type Routes = Arc<RwLock<Vec<Route>>>;

pub async fn run(config: ProxyConfig) -> Result<()> {
    // Explicitly install ring as the TLS crypto provider.
    // Without this, rustls panics when both ring and aws-lc-rs are present.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let tls_acceptor = make_tls_acceptor()?;

    let http_addr: SocketAddr = format!("0.0.0.0:{}", config.http_port).parse()?;
    let https_addr: SocketAddr = format!("0.0.0.0:{}", config.https_port).parse()?;

    let http_listener = TcpListener::bind(http_addr).await.map_err(|e| {
        anyhow::anyhow!(
            "Cannot bind HTTP port {} — try running with sudo: {e}",
            config.http_port
        )
    })?;
    let https_listener = TcpListener::bind(https_addr).await.map_err(|e| {
        anyhow::anyhow!(
            "Cannot bind HTTPS port {} — try running with sudo: {e}",
            config.https_port
        )
    })?;

    eprintln!(
        "dip-proxy: HTTP :{} → HTTPS :{}, {} route(s)",
        config.http_port,
        config.https_port,
        config.routes.len()
    );

    // Routes are in a RwLock so SIGHUP can hot-reload them without restart
    let routes: Routes = Arc::new(RwLock::new(config.routes));

    // ── SIGHUP handler — reload routes from config file ───────────────────
    {
        let routes = routes.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sig = match signal(SignalKind::hangup()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("dip-proxy: cannot register SIGHUP handler: {e}");
                    return;
                }
            };
            loop {
                sig.recv().await;
                match super::config::load() {
                    Ok(new_cfg) => {
                        let mut w = routes.write().await;
                        let n = new_cfg.routes.len();
                        *w = new_cfg.routes;
                        eprintln!("dip-proxy: routes reloaded ({n} route(s))");
                    }
                    Err(e) => eprintln!("dip-proxy: config reload failed: {e}"),
                }
            }
        });
    }

    let https_port = config.https_port;

    // ── HTTP: redirect to HTTPS ───────────────────────────────────────────
    let http_task = tokio::spawn(async move {
        loop {
            let (stream, _) = match http_listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("HTTP accept error: {e}");
                    continue;
                }
            };
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        hyper::service::service_fn(move |req| redirect_to_https(req, https_port)),
                    )
                    .await;
            });
        }
    });

    // ── HTTPS: TLS + reverse proxy (with WebSocket upgrade support) ───────
    let https_task = tokio::spawn(async move {
        loop {
            let (stream, client_addr) = match https_listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("HTTPS accept error: {e}");
                    continue;
                }
            };
            let tls_acceptor = tls_acceptor.clone();
            let routes = routes.clone();
            tokio::spawn(async move {
                let tls_stream = match tls_acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(e) => {
                        // Client-side alerts (CertificateUnknown, UnknownCa, etc.)
                        // mean the client doesn't trust our CA — not a server error.
                        // Suppress these to keep the log clean.
                        if !is_client_tls_rejection(&e.to_string()) {
                            eprintln!("TLS error: {e}");
                        }
                        return;
                    }
                };
                let io = TokioIo::new(tls_stream);
                // auto::Builder negotiates H2 or H1.1 based on ALPN.
                // serve_connection_with_upgrades keeps WebSocket working on H1.1.
                let _ = server_auto::Builder::new(TokioExecutor::new())
                    .serve_connection_with_upgrades(
                        io,
                        hyper::service::service_fn(move |req| {
                            handle_https(req, routes.clone(), client_addr)
                        }),
                    )
                    .await;
            });
        }
    });

    tokio::try_join!(
        async { http_task.await.map_err(|e| anyhow::anyhow!(e)) },
        async { https_task.await.map_err(|e| anyhow::anyhow!(e)) },
    )?;

    Ok(())
}

// ─── handlers ────────────────────────────────────────────────────────────────

async fn redirect_to_https(
    req: Request<Incoming>,
    https_port: u16,
) -> Result<Response<Full<Bytes>>, BoxError> {
    let host = req
        .headers()
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost")
        .split(':')
        .next()
        .unwrap_or("localhost");

    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");

    let location = if https_port == 443 {
        format!("https://{host}{path}")
    } else {
        format!("https://{host}:{https_port}{path}")
    };

    Ok(Response::builder()
        .status(StatusCode::MOVED_PERMANENTLY)
        .header("Location", location)
        .body(Full::new(Bytes::new()))
        .unwrap())
}

async fn handle_https(
    req: Request<Incoming>,
    routes: Routes,
    client_addr: SocketAddr,
) -> Result<Response<Full<Bytes>>, BoxError> {
    let host = extract_host(&req);

    let upstream = {
        let r = routes.read().await;
        router::match_route(&host, &r).map(|r| r.upstream.clone())
    };

    let Some(upstream) = upstream else {
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("content-type", "text/plain")
            .body(Full::new(Bytes::from(format!(
                "dip-proxy: no route for '{host}'\n"
            ))))
            .unwrap());
    };

    // WebSocket / protocol upgrade — tunnel raw TCP after handshake
    if is_upgrade_request(&req) {
        return match proxy_upgrade(req, &upstream).await {
            Ok(r) => Ok(r),
            Err(e) => {
                eprintln!("upgrade error [{host} → {upstream}]: {e}");
                Ok(error_resp(
                    StatusCode::BAD_GATEWAY,
                    format!("dip-proxy: upgrade error: {e}\n"),
                ))
            }
        };
    }

    let method = req.method().clone();
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());

    let started = std::time::Instant::now();

    let result = proxy_http(req, &upstream, &host, client_addr).await;

    match result {
        Ok(resp) => {
            access_log(&method, &host, &path, resp.status(), started.elapsed());
            Ok(resp)
        }
        Err(e) => {
            eprintln!("proxy error [{host} → {upstream}]: {e}");
            Ok(error_resp(
                StatusCode::BAD_GATEWAY,
                format!("dip-proxy: upstream error: {e}\n"),
            ))
        }
    }
}

// ─── host extraction ─────────────────────────────────────────────────────────

/// Extract the bare hostname (no port) from a request.
///
/// HTTP/1.1 uses the `Host` header.
/// HTTP/2 uses the `:authority` pseudo-header which hyper exposes via `req.uri().host()`.
/// We try both so routing works regardless of protocol version.
fn extract_host<B>(req: &Request<B>) -> String {
    // HTTP/1.1: Host header
    if let Some(host) = req
        .headers()
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
    {
        return host.split(':').next().unwrap_or(host).to_string();
    }
    // HTTP/2: :authority pseudo-header → req.uri().host()
    if let Some(host) = req.uri().host() {
        return host.to_string();
    }
    String::new()
}

// ─── plain HTTP proxy ─────────────────────────────────────────────────────────

async fn proxy_http(
    req: Request<Incoming>,
    upstream: &str,
    host: &str,
    client_addr: SocketAddr,
) -> Result<Response<Full<Bytes>>, BoxError> {
    let stream = TcpStream::connect(upstream).await?;
    let io = TokioIo::new(stream);

    let (mut sender, conn) = client_http1::Builder::new().handshake(io).await?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("upstream conn dropped: {e}");
        }
    });

    let (parts, body) = req.into_parts();
    let body_bytes = body.collect().await?.to_bytes();

    let path = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_string();

    let mut upstream_req = Request::builder()
        .method(parts.method)
        .uri(&path)
        .body(Full::new(body_bytes))?;

    // Strip hop-by-hop headers (RFC 7230 §6.1) — not forwarded to upstream
    let mut headers = parts.headers;
    for name in &[
        "connection",
        "keep-alive",
        "proxy-connection",
        "transfer-encoding",
        "te",
        "upgrade",
        "proxy-authorization",
        "proxy-authenticate",
        "trailer",
    ] {
        headers.remove(*name);
    }
    // HTTP/2 uses :authority instead of Host; upstream is always HTTP/1.1
    // and nginx requires Host for server_name matching — inject original domain.
    if !headers.contains_key(hyper::header::HOST)
        && let Ok(val) = hyper::header::HeaderValue::from_str(host)
    {
        headers.insert(hyper::header::HOST, val);
    }

    // Forwarding headers so apps know the real client IP and original protocol.
    // Without these: Laravel/Symfony generate http:// URLs, rate limiting breaks,
    // request()->ip() returns the proxy IP instead of the browser.
    let client_ip = client_addr.ip().to_string();
    // X-Forwarded-For: append to existing chain if present (multi-proxy setups)
    let xff = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .map(|existing| format!("{existing}, {client_ip}"))
        .unwrap_or_else(|| client_ip.clone());
    if let Ok(val) = hyper::header::HeaderValue::from_str(&xff) {
        headers.insert("x-forwarded-for", val);
    }
    if let Ok(val) = hyper::header::HeaderValue::from_str(&client_ip) {
        headers.insert("x-real-ip", val);
    }
    headers.insert(
        "x-forwarded-proto",
        hyper::header::HeaderValue::from_static("https"),
    );
    if let Ok(val) = hyper::header::HeaderValue::from_str(host) {
        headers.insert("x-forwarded-host", val);
    }

    *upstream_req.headers_mut() = headers;

    let resp = sender.send_request(upstream_req).await?;
    let (resp_parts, resp_body) = resp.into_parts();
    let resp_bytes = resp_body.collect().await?.to_bytes();

    Ok(Response::from_parts(resp_parts, Full::new(resp_bytes)))
}

// ─── WebSocket / upgrade tunnel ───────────────────────────────────────────────

/// Returns true if the request carries `Connection: upgrade`.
fn is_upgrade_request(req: &Request<Incoming>) -> bool {
    req.headers()
        .get(hyper::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("upgrade"))
        .unwrap_or(false)
}

/// Forward an HTTP upgrade (WebSocket, etc.) to the upstream.
///
/// Steps:
///   1. Grab client upgrade future BEFORE consuming the request.
///   2. Forward the request to upstream (keeping upgrade headers).
///   3. If upstream responds 101, spawn a bidirectional TCP tunnel.
///   4. Return the 101 response to the client — hyper handles the rest.
async fn proxy_upgrade(
    mut req: Request<Incoming>,
    upstream: &str,
) -> Result<Response<Full<Bytes>>, BoxError> {
    // Must take the upgrade future before into_parts() consumes the request
    let client_upgrade = hyper::upgrade::on(&mut req);

    let (parts, body) = req.into_parts();
    let body_bytes = body.collect().await?.to_bytes();
    let path = parts
        .uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_string();

    // Build the forwarded request — keep ALL headers (including upgrade-related ones)
    let mut upstream_req = Request::builder()
        .method(&parts.method)
        .uri(&path)
        .body(Full::new(body_bytes))?;
    *upstream_req.headers_mut() = parts.headers;

    // Connect to upstream — must use with_upgrades() so hyper tunnels properly
    let stream = TcpStream::connect(upstream).await?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = client_http1::Builder::new().handshake(io).await?;
    tokio::spawn(async move {
        if let Err(e) = conn.with_upgrades().await {
            eprintln!("upstream upgrade conn error: {e}");
        }
    });

    let mut upstream_resp = sender.send_request(upstream_req).await?;

    if upstream_resp.status() == StatusCode::SWITCHING_PROTOCOLS {
        let upstream_upgrade = hyper::upgrade::on(&mut upstream_resp);

        // Tunnel raw bytes between client and upstream in both directions
        tokio::spawn(async move {
            match tokio::try_join!(client_upgrade, upstream_upgrade) {
                Ok((client_io, upstream_io)) => {
                    let mut client = TokioIo::new(client_io);
                    let mut upstream = TokioIo::new(upstream_io);
                    if let Err(e) = tokio::io::copy_bidirectional(&mut client, &mut upstream).await
                    {
                        // Ignore "connection reset" — browser closed the tab
                        let msg = e.to_string();
                        if !msg.contains("reset") && !msg.contains("broken pipe") {
                            eprintln!("WS tunnel error: {e}");
                        }
                    }
                }
                Err(e) => eprintln!("WS upgrade handshake failed: {e}"),
            }
        });

        // Return the 101 Switching Protocols to the client (empty body is correct)
        let (resp_parts, _) = upstream_resp.into_parts();
        return Ok(Response::from_parts(resp_parts, Full::new(Bytes::new())));
    }

    // Upstream didn't upgrade — proxy the response normally
    let (resp_parts, resp_body) = upstream_resp.into_parts();
    let resp_bytes = resp_body.collect().await?.to_bytes();
    Ok(Response::from_parts(resp_parts, Full::new(resp_bytes)))
}

// ─── TLS setup ───────────────────────────────────────────────────────────────

fn make_tls_acceptor() -> Result<TlsAcceptor> {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use rustls_pemfile::{certs, private_key};
    use std::io::BufReader;

    let cert_path = super::certs::srv_cert_path();
    let key_path = super::certs::srv_key_path();

    let cert_file = std::fs::File::open(&cert_path).map_err(|e| {
        anyhow::anyhow!(
            "TLS cert not found ({}): {e}\nRun: dip proxy init",
            cert_path.display()
        )
    })?;
    let key_file = std::fs::File::open(&key_path).map_err(|e| {
        anyhow::anyhow!(
            "TLS key not found ({}): {e}\nRun: dip proxy init",
            key_path.display()
        )
    })?;

    let cert_chain: Vec<CertificateDer<'static>> = certs(&mut BufReader::new(cert_file))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("Bad TLS cert: {e}"))?;

    let key: PrivateKeyDer<'static> = private_key(&mut BufReader::new(key_file))
        .map_err(|e| anyhow::anyhow!("Bad TLS key: {e}"))?
        .ok_or_else(|| anyhow::anyhow!("No private key in {}", key_path.display()))?;

    let mut tls_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| anyhow::anyhow!("TLS config error: {e}"))?;

    // Advertise HTTP/2 and HTTP/1.1 via ALPN so browsers can negotiate H2.
    // Upstream connections always use HTTP/1.1 (containers don't need H2).
    tls_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];

    Ok(TlsAcceptor::from(Arc::new(tls_config)))
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn access_log(
    method: &hyper::Method,
    host: &str,
    path: &str,
    status: StatusCode,
    elapsed: std::time::Duration,
) {
    let now = chrono::Local::now().format("%H:%M:%S");
    let ms = elapsed.as_millis();
    let status_str = status.as_u16().to_string();
    eprintln!("{now}  {method:<6} {host}{path}  {status_str}  {ms}ms");
}

/// Returns true for TLS alerts that originate from the client rejecting our cert.
/// These are normal when the client doesn't trust the dip CA and shouldn't pollute logs.
fn is_client_tls_rejection(msg: &str) -> bool {
    msg.contains("CertificateUnknown")
        || msg.contains("UnknownCa")
        || msg.contains("CertificateExpired")
        || msg.contains("BadCertificate")
        || msg.contains("received fatal alert")
}

fn error_resp(status: StatusCode, body: String) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from(body)))
        .unwrap()
}
