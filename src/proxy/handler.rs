use std::net::SocketAddr;

use bytes::Bytes;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use super::config::Route;
use super::{BoxError, PooledClient, RespBody, Routes, box_bytes, box_empty};

pub async fn redirect_to_https(
    req: Request<Incoming>,
    https_port: u16,
) -> Result<Response<RespBody>, BoxError> {
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
        .body(box_empty())
        .unwrap())
}

pub async fn handle_https(
    req: Request<Incoming>,
    routes: Routes,
    client_addr: SocketAddr,
    client: PooledClient,
) -> Result<Response<RespBody>, BoxError> {
    let host = extract_host(&req);

    let upstream = {
        let r = routes.read().await;
        super::router::match_route(&host, &r).map(|r: &Route| r.upstream.clone())
    };

    let Some(upstream) = upstream else {
        return Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("content-type", "text/plain")
            .body(box_bytes(
                format!("dip-proxy: no route for '{host}'\n").into(),
            ))
            .unwrap());
    };

    if is_upgrade_request(&req) {
        return match super::upstream::proxy_upgrade(req, &upstream).await {
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

    let result = super::upstream::proxy_http(req, &upstream, &host, client_addr, &client).await;

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

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Extract the bare hostname (no port) from a request.
///
/// HTTP/1.1 uses the `Host` header.
/// HTTP/2 uses the `:authority` pseudo-header which hyper exposes via `req.uri().host()`.
/// We try both so routing works regardless of protocol version.
pub fn extract_host<B>(req: &Request<B>) -> String {
    if let Some(host) = req
        .headers()
        .get(hyper::header::HOST)
        .and_then(|v| v.to_str().ok())
    {
        return host.split(':').next().unwrap_or(host).to_string();
    }
    if let Some(host) = req.uri().host() {
        return host.to_string();
    }
    String::new()
}

fn is_upgrade_request(req: &Request<Incoming>) -> bool {
    req.headers()
        .get(hyper::header::CONNECTION)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase().contains("upgrade"))
        .unwrap_or(false)
}

pub fn error_resp(status: StatusCode, body: String) -> Response<RespBody> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(box_bytes(Bytes::from(body)))
        .unwrap()
}

fn access_log(
    method: &hyper::Method,
    host: &str,
    path: &str,
    status: StatusCode,
    elapsed: std::time::Duration,
) {
    let now = chrono::Local::now().format("%H:%M:%S");
    let ms = elapsed.as_millis();
    eprintln!(
        "{now}  {method:<6} {host}{path}  {}  {ms}ms",
        status.as_u16()
    );
}
