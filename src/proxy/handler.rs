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

    let method = req.method().clone();
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "/".to_string());

    if is_upgrade_request(&req) {
        return match super::upstream::proxy_upgrade(req, &upstream).await {
            Ok(r) => Ok(r),
            Err(e) => {
                let detail = format_error_chain(e.as_ref());
                log_proxy_error("upgrade error", &method, &host, &path, &upstream, &detail);
                Ok(error_resp(
                    StatusCode::BAD_GATEWAY,
                    error_body("upgrade error", &detail, &upstream),
                ))
            }
        };
    }

    let started = std::time::Instant::now();

    let result = super::upstream::proxy_http(req, &upstream, &host, client_addr, &client).await;

    match result {
        Ok(resp) => {
            access_log(&method, &host, &path, resp.status(), started.elapsed());
            Ok(resp)
        }
        Err(e) => {
            let detail = format_error_chain(e.as_ref());
            log_proxy_error("proxy error", &method, &host, &path, &upstream, &detail);
            Ok(error_resp(
                StatusCode::BAD_GATEWAY,
                error_body("upstream error", &detail, &upstream),
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

fn log_proxy_error(
    label: &str,
    method: &hyper::Method,
    host: &str,
    path: &str,
    upstream: &str,
    detail: &str,
) {
    eprintln!("{label} [{method} {host}{path} -> {upstream}]: {detail}");
    if let Some(hint) = proxy_error_hint(detail, upstream) {
        eprintln!("  hint: {hint}");
    }
}

fn error_body(label: &str, detail: &str, upstream: &str) -> String {
    let mut body = format!("dip-proxy: {label}: {detail}\n");
    if let Some(hint) = proxy_error_hint(detail, upstream) {
        body.push_str("hint: ");
        body.push_str(&hint);
        body.push('\n');
    }
    body
}

fn format_error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut parts = vec![err.to_string()];
    let mut current = err.source();

    while let Some(source) = current {
        let text = source.to_string();
        if !parts.iter().any(|part| part == &text) {
            parts.push(text);
        }
        current = source.source();
    }

    parts.join(": ")
}

fn proxy_error_hint(detail: &str, upstream: &str) -> Option<String> {
    let lower = detail.to_ascii_lowercase();

    // Must come before the generic "connection refused"/"timed out" branches:
    // a dead agent also produces those strings, but the fix is different.
    if lower.contains("socks agent") {
        return Some(
            "the connection goes through whalet's SOCKS agent and it failed; \
             check `whalet status` (or set DIP_SOCKS=off to force direct connects)"
                .to_string(),
        );
    }

    if lower.contains("connection refused")
        || lower.contains("os error 61")
        || lower.contains("os error 111")
    {
        return Some(format!(
            "upstream refused TCP connection; check that the service is running and listening on {upstream}, then run `dip proxy sync` if the container IP changed"
        ));
    }

    if lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("operation timed out")
    {
        return Some(format!(
            "connection to {upstream} timed out; the container network may be unreachable or the service may be stuck accepting connections"
        ));
    }

    if lower.contains("no route to host")
        || lower.contains("network is unreachable")
        || lower.contains("host is down")
    {
        return Some(format!(
            "cannot reach {upstream}; the route may point at a stale container IP, so try `dip proxy sync` or restart the project"
        ));
    }

    if lower.contains("invalid uri") {
        return Some(format!(
            "proxy route target `{upstream}` is not a valid host:port upstream"
        ));
    }

    None
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
    let now = crate::utils::local_hms();
    let ms = elapsed.as_millis();
    eprintln!(
        "{now}  {method:<6} {host}{path}  {}  {ms}ms",
        status.as_u16()
    );
}

#[cfg(test)]
mod tests {
    use std::fmt;

    use super::*;

    #[derive(Debug)]
    struct ChainError {
        message: &'static str,
        source: Option<Box<ChainError>>,
    }

    impl fmt::Display for ChainError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(self.message)
        }
    }

    impl std::error::Error for ChainError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source
                .as_deref()
                .map(|source| source as &(dyn std::error::Error + 'static))
        }
    }

    #[test]
    fn format_error_chain_includes_sources() {
        let err = ChainError {
            message: "client error (Connect)",
            source: Some(Box::new(ChainError {
                message: "tcp connect error",
                source: Some(Box::new(ChainError {
                    message: "Connection refused (os error 61)",
                    source: None,
                })),
            })),
        };

        assert_eq!(
            format_error_chain(&err),
            "client error (Connect): tcp connect error: Connection refused (os error 61)"
        );
    }

    #[test]
    fn proxy_error_hint_explains_refused_connections() {
        let hint = proxy_error_hint(
            "client error: Connection refused (os error 61)",
            "127.0.0.1:3000",
        )
        .unwrap();

        assert!(hint.contains("upstream refused TCP connection"));
        assert!(hint.contains("127.0.0.1:3000"));
    }
}
