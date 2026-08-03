use std::net::SocketAddr;
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::{Body as _, Incoming};
use hyper::client::conn::http1 as client_http1;
use hyper::header::HeaderMap;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

use super::{BoxError, PooledClient, RespBody, box_bytes, box_empty};

/// Upstream TCP connect budget. Upstreams are local containers: if a connect
/// takes seconds, the container is gone — fail fast with a clear error
/// instead of letting the browser hang on the ~75s OS TCP timeout.
/// Shared with the pooled client's connector (see server.rs).
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Request bodies up to this size are buffered so the pooled client can
/// safely retry on a stale keep-alive connection. Larger bodies (and chunked
/// bodies of unknown size) stream through a dedicated connection — a 2 GB
/// upload must not require 2 GB of proxy RSS.
const MAX_BUFFERED_BODY: u64 = 16 * 1024 * 1024;

pub async fn proxy_http(
    req: Request<Incoming>,
    upstream: &str,
    host: &str,
    client_addr: SocketAddr,
    client: &PooledClient,
) -> Result<Response<RespBody>, BoxError> {
    let (parts, body) = req.into_parts();

    let path = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/")
        .to_string();

    let headers = forward_headers(parts.headers, host, client_addr);

    // Content-Length unknown (chunked) or large → stream, don't buffer.
    let body_size = body.size_hint().exact();
    if body_size.is_none_or(|n| n > MAX_BUFFERED_BODY) {
        return proxy_http_streaming(parts.method, &path, headers, body, upstream).await;
    }

    // Collect request body — small ones only (see MAX_BUFFERED_BODY). The
    // pool may need to retry on a stale connection, which requires the body
    // to still be available.
    let body_bytes = body.collect().await?.to_bytes();

    // Absolute URI so the pooled client knows which host to connect to.
    let uri: hyper::Uri = format!("http://{upstream}{path}").parse()?;

    let mut upstream_req = Request::builder()
        .method(parts.method)
        .uri(uri)
        .body(Full::new(body_bytes))?;
    *upstream_req.headers_mut() = headers;

    let resp = client.request(upstream_req).await?;
    let (resp_parts, resp_body) = resp.into_parts();

    // Stream the response body instead of buffering — large file downloads,
    // SSE, and chunked transfers flow through without holding everything in RAM.
    let streamed = resp_body.map_err(|e| -> BoxError { Box::new(e) }).boxed();
    Ok(Response::from_parts(resp_parts, streamed))
}

/// Large/chunked request bodies bypass the pool: a dedicated connection lets
/// the body stream straight through. No retry is possible (the body can only
/// be read once), but for uploads that trade-off is correct.
async fn proxy_http_streaming(
    method: Method,
    path: &str,
    headers: HeaderMap,
    body: Incoming,
    upstream: &str,
) -> Result<Response<RespBody>, BoxError> {
    let stream = connect_upstream(upstream).await?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = client_http1::Builder::new().handshake(io).await?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("upstream streaming conn error: {e}");
        }
    });

    // Origin-form URI: this connection already points at the right upstream.
    let mut upstream_req = Request::builder().method(method).uri(path).body(body)?;
    *upstream_req.headers_mut() = headers;

    let resp = sender.send_request(upstream_req).await?;
    let (resp_parts, resp_body) = resp.into_parts();
    let streamed = resp_body.map_err(|e| -> BoxError { Box::new(e) }).boxed();
    Ok(Response::from_parts(resp_parts, streamed))
}

/// Forward an HTTP upgrade (WebSocket, etc.) to the upstream.
///
/// Steps:
///   1. Grab client upgrade future BEFORE consuming the request.
///   2. Forward the request to upstream (keeping upgrade headers).
///   3. If upstream responds 101, spawn a bidirectional TCP tunnel.
///   4. Return the 101 response to the client — hyper handles the rest.
pub async fn proxy_upgrade(
    mut req: Request<Incoming>,
    upstream: &str,
) -> Result<Response<RespBody>, BoxError> {
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

    // WebSocket upgrades need a dedicated connection, not a pooled keep-alive one.
    // After the 101 handshake the connection becomes a raw TCP tunnel.
    let stream = connect_upstream(upstream).await?;
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

        let (resp_parts, _) = upstream_resp.into_parts();
        return Ok(Response::from_parts(resp_parts, box_empty()));
    }

    // Upstream didn't upgrade — proxy the response normally
    let (resp_parts, resp_body) = upstream_resp.into_parts();
    let resp_bytes = resp_body.collect().await?.to_bytes();
    Ok(Response::from_parts(resp_parts, box_bytes(resp_bytes)))
}

/// TCP connect with a hard deadline (see CONNECT_TIMEOUT). Dials direct or
/// through the SOCKS agent depending on what the dialer probe last saw.
async fn connect_upstream(upstream: &str) -> Result<TcpStream, BoxError> {
    match tokio::time::timeout(CONNECT_TIMEOUT, super::dialer::current().connect(upstream)).await {
        Ok(Ok(stream)) => Ok(stream),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(format!(
            "connect to {upstream} timed out after {}s (container gone?)",
            CONNECT_TIMEOUT.as_secs()
        )
        .into()),
    }
}

/// Hop-by-hop header stripping (RFC 7230 §6.1) plus the forwarding headers
/// apps rely on. Shared by the pooled and streaming paths.
///
/// Without X-Forwarded-*: Laravel/Symfony generate http:// URLs, rate
/// limiting breaks, request()->ip() returns the proxy IP instead of the
/// browser.
fn forward_headers(mut headers: HeaderMap, host: &str, client_addr: SocketAddr) -> HeaderMap {
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
    headers
}
