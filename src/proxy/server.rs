use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1 as server_http1;
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ServerConfig;
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tokio_rustls::TlsAcceptor;

use super::config::ProxyConfig;
use super::{PooledClient, Routes};

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

    // Clone routes for the watcher before https_task consumes them via `move`
    let watcher_routes = routes.clone();

    // One shared HTTP/1.1 client for all proxy requests — reuses TCP connections
    // per upstream via keep-alive, avoiding a new TCP handshake on every request.
    let http_client: PooledClient = Arc::new(
        Client::builder(TokioExecutor::new()).build::<_, Full<Bytes>>(HttpConnector::new()),
    );

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
                        hyper::service::service_fn(move |req| {
                            super::handler::redirect_to_https(req, https_port)
                        }),
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
            let http_client = http_client.clone();
            tokio::spawn(async move {
                let tls_stream = match tls_acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(e) => {
                        if !is_client_tls_rejection(&e.to_string()) {
                            eprintln!("TLS error: {e}");
                        }
                        return;
                    }
                };
                let io = TokioIo::new(tls_stream);
                // HTTP/1.1 only: a local dev proxy gains nothing from h2, and
                // dropping it removes the whole h2 stack from the binary.
                // with_upgrades() keeps WebSocket working.
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        io,
                        hyper::service::service_fn(move |req| {
                            super::handler::handle_https(
                                req,
                                routes.clone(),
                                client_addr,
                                http_client.clone(),
                            )
                        }),
                    )
                    .with_upgrades()
                    .await;
            });
        }
    });

    // ── Docker event watcher — auto-syncs routes on container start/stop ─
    let watcher_task = tokio::spawn(async move {
        super::watcher::run(watcher_routes).await;
    });

    // ── Built-in DNS server ───────────────────────────────────────────────
    let dns_task = {
        let dns_port = config.dns_port;
        let tlds = config.tlds.clone();
        let upstream_dns = config
            .upstream_dns
            .first()
            .map(|s| format!("{s}:53"))
            .unwrap_or_else(|| "8.8.8.8:53".to_string());
        tokio::spawn(async move {
            if let Err(e) = crate::dns::server::run(dns_port, tlds, upstream_dns).await {
                eprintln!("dip-dns: {e}");
            }
        })
    };

    tokio::try_join!(
        async { http_task.await.map_err(|e| anyhow::anyhow!(e)) },
        async { https_task.await.map_err(|e| anyhow::anyhow!(e)) },
        async { dns_task.await.map_err(|e| anyhow::anyhow!(e)) },
        async { watcher_task.await.map_err(|e| anyhow::anyhow!(e)) },
    )?;

    Ok(())
}

// ─── TLS ─────────────────────────────────────────────────────────────────────

fn make_tls_acceptor() -> Result<TlsAcceptor> {
    use rustls_pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

    let cert_path = super::certs::srv_cert_path();
    let key_path = super::certs::srv_key_path();

    let cert_chain: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(&cert_path)
        .map_err(|e| {
            anyhow::anyhow!(
                "TLS cert not found ({}): {e}\nRun: dip proxy init",
                cert_path.display()
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow::anyhow!("Bad TLS cert: {e}"))?;

    let key: PrivateKeyDer<'static> = PrivateKeyDer::from_pem_file(&key_path).map_err(|e| {
        anyhow::anyhow!(
            "TLS key not found or invalid ({}): {e}\nRun: dip proxy init",
            key_path.display()
        )
    })?;

    let mut tls_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| anyhow::anyhow!("TLS config error: {e}"))?;

    tls_config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(tls_config)))
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
