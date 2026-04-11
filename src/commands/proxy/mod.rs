//! Built-in TLS reverse proxy.
//!
//! Workflow:
//!   dip proxy init          — generate CA + wildcard cert, install CA in keychain
//!   dip start               — containers up + proxy auto-synced from docker labels
//!   dip proxy routes        — inspect current routing table
//!   dip proxy status        — daemon status
//!   sudo dip proxy start    — start daemon manually (ports 80/443 need root)
//!
//! Routes are discovered from docker-compose labels:
//!   labels:
//!     dip.host: "${DOMAIN}:80"   →  "backend.foo.test:80"
//!
//! Module layout:
//!   setup.rs  — init + config
//!   daemon.rs — start/stop/logs/status/serve + pid/port helpers
//!   routes.rs — add/remove/sync + label parsing + docker discovery

mod daemon;
mod routes;
mod setup;

// ── re-export public API ──────────────────────────────────────────────────────

// Setup
pub use setup::{run_config, run_init};

// Daemon lifecycle
pub use daemon::{run_logs, run_serve, run_start, run_status, run_stop};

// Route management
pub use routes::{apply_sync, apply_unsync, run_add, run_remove, run_routes, run_sync};
