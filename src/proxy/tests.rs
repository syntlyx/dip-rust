//! Integration tests for the proxy pipeline on real sockets:
//! client → http1 server running `handle_https` → upstream http1 server.
//!
//! TLS is deliberately not part of these tests (the acceptor is a thin
//! rustls wrapper verified manually); what runs here is everything under
//! it — routing, header forwarding, the pooled vs streaming body paths,
//! and the connect-timeout behavior.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use hyper_util::rt::{TokioExecutor, TokioIo};
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use super::config::Route;
use super::{PooledClient, RespBody, Routes, box_bytes};

/// Upstream: GET / → "ok"; POST /upload → drains the body incrementally
/// and reports how many bytes arrived.
async fn upstream_service(req: Request<Incoming>) -> Result<Response<RespBody>, super::BoxError> {
    match req.uri().path() {
        "/upload" => {
            let mut body = req.into_body();
            let mut total: usize = 0;
            while let Some(frame) = body.frame().await {
                if let Some(data) = frame?.data_ref() {
                    total += data.len();
                }
            }
            Ok(Response::new(box_bytes(format!("got {total}").into())))
        }
        _ => Ok(Response::new(box_bytes(Bytes::from_static(b"ok")))),
    }
}

async fn spawn_upstream() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service_fn(upstream_service))
                    .await;
            });
        }
    });
    addr
}

/// The proxy frontend: plain http1 (no TLS) running the real handler.
async fn spawn_proxy(routes: Routes, connect_timeout: Duration) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // The dialer's global probe state is never initialized in tests, so
    // DipConnector always dials direct here — same as production on Linux.
    let connector = super::dialer::DipConnector::new(connect_timeout);
    let client: PooledClient =
        Arc::new(Client::builder(TokioExecutor::new()).build::<_, Full<Bytes>>(connector));

    tokio::spawn(async move {
        loop {
            let (stream, client_addr) = match listener.accept().await {
                Ok(v) => v,
                Err(_) => break,
            };
            let routes = routes.clone();
            let client = client.clone();
            tokio::spawn(async move {
                let _ = server_http1::Builder::new()
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(move |req| {
                            super::handler::handle_https(
                                req,
                                routes.clone(),
                                client_addr,
                                client.clone(),
                            )
                        }),
                    )
                    .await;
            });
        }
    });
    addr
}

fn routes_for(domain: &str, upstream: SocketAddr) -> Routes {
    Arc::new(RwLock::new(vec![Route {
        domain: domain.to_string(),
        upstream: upstream.to_string(),
    }]))
}

fn test_client() -> Client<HttpConnector, Full<Bytes>> {
    Client::builder(TokioExecutor::new()).build::<_, Full<Bytes>>(HttpConnector::new())
}

#[tokio::test(flavor = "multi_thread")]
async fn routes_request_and_forwards_headers() {
    let upstream = spawn_upstream().await;
    let proxy = spawn_proxy(routes_for("app.test", upstream), Duration::from_secs(5)).await;

    let req = Request::builder()
        .uri(format!("http://{proxy}/"))
        .header("host", "app.test")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = test_client().request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_host_gets_404_not_hang() {
    let upstream = spawn_upstream().await;
    let proxy = spawn_proxy(routes_for("app.test", upstream), Duration::from_secs(5)).await;

    let req = Request::builder()
        .uri(format!("http://{proxy}/"))
        .header("host", "nobody.test")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = test_client().request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

/// Bodies over MAX_BUFFERED_BODY take the dedicated streaming path; the
/// upstream must receive every byte.
#[tokio::test(flavor = "multi_thread")]
async fn large_upload_streams_through_completely() {
    let upstream = spawn_upstream().await;
    let proxy = spawn_proxy(routes_for("app.test", upstream), Duration::from_secs(5)).await;

    let size = 20 * 1024 * 1024; // > 16MB threshold → streaming path
    let payload = Bytes::from(vec![0x42u8; size]);
    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{proxy}/upload"))
        .header("host", "app.test")
        .body(Full::new(payload))
        .unwrap();
    let resp = test_client().request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(String::from_utf8_lossy(&body), format!("got {size}"));
}

/// Small bodies stay on the pooled path — same correctness guarantee.
#[tokio::test(flavor = "multi_thread")]
async fn small_upload_goes_through_pool() {
    let upstream = spawn_upstream().await;
    let proxy = spawn_proxy(routes_for("app.test", upstream), Duration::from_secs(5)).await;

    let req = Request::builder()
        .method("POST")
        .uri(format!("http://{proxy}/upload"))
        .header("host", "app.test")
        .body(Full::new(Bytes::from_static(b"hello")))
        .unwrap();
    let resp = test_client().request(req).await.unwrap();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(String::from_utf8_lossy(&body), "got 5");
}

/// A dead upstream must produce a 502 within the connect timeout, not hang
/// until the OS gives up (~75s).
#[tokio::test(flavor = "multi_thread")]
async fn dead_upstream_fails_fast_with_502() {
    // Non-routable TEST-NET-1 address: connects hang until a timeout fires.
    let dead: SocketAddr = "192.0.2.1:81".parse().unwrap();
    let proxy = spawn_proxy(routes_for("dead.test", dead), Duration::from_millis(500)).await;

    let started = std::time::Instant::now();
    let req = Request::builder()
        .uri(format!("http://{proxy}/"))
        .header("host", "dead.test")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = test_client().request(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "should fail via connect timeout, took {:?}",
        started.elapsed()
    );
}
