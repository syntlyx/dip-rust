pub mod docker;

pub use docker::DockerRuntime;

use anyhow::Result;

use crate::project::ProjectConfig;

// ─── trait ────────────────────────────────────────────────────────────────────

/// Abstraction over container runtimes (Docker Compose, Apple Container, …).
///
/// Each runtime implements this trait and translates the compose-centric API
/// into whatever native commands it supports. Commands never talk to a runtime
/// directly — they always go through `Runtime`.
pub trait ContainerRuntime: Send + Sync {
    fn check_daemon(&self) -> Result<()>;

    /// Run a compose command, show a spinner, stream output above it.
    fn compose_run(
        &self,
        project: &ProjectConfig,
        args: &[&str],
        msg: &str,
        verbose: bool,
        no_color: bool,
    ) -> Result<()>;

    /// Run a compose command with full stdio passthrough (interactive / logs).
    fn compose_stream(&self, project: &ProjectConfig, args: &[&str], verbose: bool) -> Result<()>;

    /// Run a compose command and capture stdout as a String.
    fn compose_capture(
        &self,
        project: &ProjectConfig,
        args: &[&str],
        verbose: bool,
    ) -> Result<String>;

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

    // ── compose delegates ──────────────────────────────────────────────────

    pub fn compose_run(&self, args: &[&str], msg: &str) -> Result<()> {
        self.backend
            .compose_run(&self.project, args, msg, self.verbose, self.no_color)
    }

    pub fn compose_stream(&self, args: &[&str]) -> Result<()> {
        self.backend
            .compose_stream(&self.project, args, self.verbose)
    }

    pub fn compose_capture(&self, args: &[&str]) -> Result<String> {
        self.backend
            .compose_capture(&self.project, args, self.verbose)
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
/// Prefers Docker (established + compose support); falls back to Apple Container.
fn detect() -> Box<dyn ContainerRuntime> {
    if cmd_exists("docker") {
        return Box::new(DockerRuntime);
    }
    // Default — will produce a clear error at check_daemon()
    Box::new(DockerRuntime)
}

pub fn cmd_exists(cmd: &str) -> bool {
    use std::process::Stdio;
    std::process::Command::new("which")
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
