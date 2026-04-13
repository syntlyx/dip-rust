pub mod docker;

pub use docker::DockerRuntime;

use anyhow::Result;

use crate::project::ProjectConfig;

// ─── backend context ──────────────────────────────────────────────────────────

/// Execution context passed to `ContainerRuntime` methods.
///
/// Bundles the three parameters that every compose call needs so trait
/// signatures stay compact. Commands never construct this directly — the
/// `Runtime` wrapper creates it from its own fields.
pub(crate) struct BackendCtx<'a> {
    pub project: &'a ProjectConfig,
    pub verbose: bool,
    pub no_color: bool,
}

// ─── trait ────────────────────────────────────────────────────────────────────

/// Abstraction over container runtimes (Docker Compose, Apple Container, …).
///
/// Each runtime implements this trait and translates the compose-centric API
/// into whatever native commands it supports. Commands never talk to a runtime
/// directly — they always go through `Runtime`.
pub trait ContainerRuntime: Send + Sync {
    fn check_daemon(&self) -> Result<()>;

    /// Run a compose command, show a spinner, stream output above it.
    fn compose_run(&self, ctx: &BackendCtx, args: &[&str], msg: &str) -> Result<()>;

    /// Run a compose command with full stdio passthrough (interactive / logs).
    fn compose_stream(&self, ctx: &BackendCtx, args: &[&str]) -> Result<()>;

    /// Stream compose output, printing only lines that match any of `keywords` (case-insensitive).
    fn compose_stream_grep(&self, ctx: &BackendCtx, args: &[&str], keywords: &[&str])
    -> Result<()>;

    /// Run a compose command and capture stdout as a String.
    fn compose_capture(&self, ctx: &BackendCtx, args: &[&str]) -> Result<String>;

    /// Run a raw runtime command and capture stdout.
    fn raw_capture(&self, args: &[&str]) -> Result<String>;

    /// Run a raw runtime command with full stdio passthrough.
    fn raw_stream(&self, args: &[&str]) -> Result<()>;
}

// ─── Runtime wrapper ─────────────────────────────────────────────────────────

/// The main handle commands use. Holds project config + detected backend.
pub struct Runtime {
    backend: Box<dyn ContainerRuntime>,
    pub project: ProjectConfig,
    pub verbose: bool,
    no_color: bool,
}

impl Runtime {
    pub fn new(project: ProjectConfig, verbose: bool, no_color: bool) -> Self {
        Self {
            backend: detect(),
            project,
            verbose,
            no_color,
        }
    }

    /// Check that the active runtime daemon is reachable.
    pub fn check_daemon() -> Result<()> {
        detect().check_daemon()
    }

    fn ctx(&self) -> BackendCtx<'_> {
        BackendCtx {
            project: &self.project,
            verbose: self.verbose,
            no_color: self.no_color,
        }
    }

    // ── compose delegates ──────────────────────────────────────────────────

    pub fn compose_run(&self, args: &[&str], msg: &str) -> Result<()> {
        self.backend.compose_run(&self.ctx(), args, msg)
    }

    pub fn compose_stream(&self, args: &[&str]) -> Result<()> {
        self.backend.compose_stream(&self.ctx(), args)
    }

    pub fn compose_stream_grep(&self, args: &[&str], keywords: &[&str]) -> Result<()> {
        self.backend
            .compose_stream_grep(&self.ctx(), args, keywords)
    }

    pub fn compose_capture(&self, args: &[&str]) -> Result<String> {
        self.backend.compose_capture(&self.ctx(), args)
    }

    pub fn raw_capture(&self, args: &[&str]) -> Result<String> {
        self.backend.raw_capture(args)
    }

    pub fn raw_stream(&self, args: &[&str]) -> Result<()> {
        self.backend.raw_stream(args)
    }

    /// Return the container ID for a running compose service.
    pub fn get_container_id(&self, service: &str) -> Result<String> {
        let raw = self.compose_capture(&["ps", "-q", service])?;
        let id = raw.trim().to_string();
        if id.is_empty() {
            anyhow::bail!(
                "No running container for service '{}'. Is it started?",
                service
            );
        }
        Ok(id)
    }
}

// ─── detection ────────────────────────────────────────────────────────────────

/// Auto-detect which runtime is available.
///
/// Currently only Docker is supported. If `docker` is not found on PATH the
/// function still returns a `DockerRuntime` — `check_daemon()` will then emit
/// a clear "Docker daemon is not running" error at the call site.
fn detect() -> Box<dyn ContainerRuntime> {
    Box::new(DockerRuntime)
}
