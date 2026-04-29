pub mod certs;
pub mod config;
pub mod handler;
pub mod router;
pub mod server;
pub mod upstream;
pub mod watcher;

use std::sync::Arc;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper_util::client::legacy::{Client, connect::HttpConnector};
use tokio::sync::RwLock;

use config::Route;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;
pub type Routes = Arc<RwLock<Vec<Route>>>;
pub type RespBody = BoxBody<Bytes, BoxError>;
pub type PooledClient = Arc<Client<HttpConnector, Full<Bytes>>>;

pub(crate) fn box_bytes(b: Bytes) -> RespBody {
    Full::new(b)
        .map_err(|_: std::convert::Infallible| -> BoxError { unreachable!() })
        .boxed()
}

pub(crate) fn box_empty() -> RespBody {
    box_bytes(Bytes::new())
}
