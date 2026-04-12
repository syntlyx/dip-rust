#!/bin/sh
set -e

# Scaffold a minimal Axum project if nothing is mounted yet
if [ ! -f "/app/Cargo.toml" ]; then
  echo "[entrypoint] No Rust project found — scaffolding..."

  cargo init /app --name ${PROJECT_NAME}

  cat >/app/Cargo.toml <<EOF
[package]
name = "${PROJECT_NAME}"
version = "0.1.0"
edition = "2021"

[dependencies]
axum = "0.8"
tokio = { version = "1", features = ["full"] }
sqlx = { version = "0.8", features = ["postgres", "runtime-tokio-native-tls", "migrate"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
dotenvy = "0.15"
tracing = "0.1"
tracing-subscriber = "0.3"
EOF

  cat >/app/src/main.rs <<'RSEOF'
use axum::{routing::get, Router};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    tracing::info!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

async fn root() -> &'static str {
    "Hello from Axum!"
}

async fn health() -> &'static str {
    r#"{"status":"ok"}"#
}
RSEOF

  echo "[entrypoint] Scaffold done. First build will take a few minutes..."
fi

exec "$@"
