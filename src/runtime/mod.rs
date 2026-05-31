#[cfg(target_os = "macos")]
pub mod apple;
pub mod compose_file;
pub mod docker;

#[cfg(target_os = "macos")]
pub use apple::AppleContainerRuntime;
pub use docker::DockerRuntime;

use anyhow::Result;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::db::DbService;
use crate::project::ProjectConfig;
use crate::utils::output::Output;
use serde_json::Value;

// ─── shared runtime data ─────────────────────────────────────────────────────

#[derive(Clone)]
pub struct RuntimeBenchSpec {
    pub iterations: usize,
    pub warmup: usize,
    pub image: String,
    pub path: String,
    pub size_mb: u64,
    pub mount: Option<RuntimeBenchMount>,
}

#[derive(Clone)]
pub struct RuntimeBenchMount {
    pub host_path: PathBuf,
    pub container_path: String,
}

pub struct RuntimeBenchMeasurement {
    pub runtime: String,
    pub image: String,
    pub iterations: usize,
    pub warmup: usize,
    pub start_ms: f64,
    pub start_total_ms: f64,
    pub exec_ms: f64,
    pub exec_total_ms: f64,
    pub disk_ms: f64,
    pub disk_total_ms: f64,
    pub disk_mib_per_s: f64,
    pub total_ms: f64,
}

#[derive(Default)]
pub struct RuntimeBenchTotals {
    pub start: Duration,
    pub exec: Duration,
    pub disk: Duration,
}

pub struct RuntimeSystemInfo {
    pub version_label: &'static str,
    pub version: String,
    pub images: Option<String>,
    pub containers: Option<RuntimeContainerCounts>,
}

pub struct RuntimeContainerCounts {
    pub total: String,
    pub running: String,
    pub paused: String,
    pub stopped: String,
}

pub struct RuntimeProjectContainer {
    pub labels: Value,
    pub ip: Option<String>,
}

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
    fn name(&self) -> &'static str;

    fn check_daemon(&self) -> Result<()>;

    /// Run a compose command, show a spinner, stream output above it.
    fn compose_run(&self, ctx: &BackendCtx, args: &[&str], msg: &str) -> Result<()>;

    /// Run a compose command with full stdio passthrough (interactive / logs).
    fn compose_stream(&self, ctx: &BackendCtx, args: &[&str], passthrough: bool) -> Result<i32>;

    /// Stream compose output, printing only lines that match any of `keywords` (case-insensitive).
    fn compose_stream_grep(&self, ctx: &BackendCtx, args: &[&str], keywords: &[&str])
    -> Result<()>;

    /// Run a compose command and capture stdout as a String.
    fn compose_capture(&self, ctx: &BackendCtx, args: &[&str]) -> Result<String>;

    /// Run a raw runtime command and capture stdout.
    fn raw_capture(&self, args: &[&str]) -> Result<String>;

    /// Run a raw runtime command with full stdio passthrough.
    fn raw_stream(&self, args: &[&str]) -> Result<()>;

    /// Inspect running project containers and return database services.
    fn db_services(&self, ctx: &BackendCtx) -> Result<Vec<DbService>>;

    /// Return project containers with labels and primary IP for proxy route discovery.
    fn project_containers(
        &self,
        ctx: &BackendCtx,
        include_stopped: bool,
    ) -> Result<Vec<RuntimeProjectContainer>>;

    /// Count running containers by Compose project label.
    fn running_count_by_project(&self, project_name: &str) -> usize;

    /// Return the primary IPv4 address for a container.
    fn container_ip(&self, container_id: &str) -> Result<String>;

    /// Remove unused system resources for this runtime.
    fn prune_plan(&self, volumes: bool, all: bool) -> Vec<&'static str>;
    fn prune_system(&self, volumes: bool, all: bool, out: &Output) -> Result<()>;

    /// Run a self-contained benchmark in a disposable test container.
    fn bench(&self, spec: &RuntimeBenchSpec) -> Result<RuntimeBenchMeasurement>;

    /// Run steady-state samples inside one long-lived disposable test container.
    fn bench_steady(&self, spec: &RuntimeBenchSpec) -> Result<RuntimeBenchMeasurement>;

    /// Return runtime version and high-level resource counts.
    fn system_info(&self) -> Result<RuntimeSystemInfo>;
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
        let backend = detect_for_project(&project);
        Self {
            backend,
            project,
            verbose,
            no_color,
        }
    }

    /// Check that the active runtime daemon is reachable.
    pub fn check_daemon() -> Result<()> {
        detect_for_current_dir().check_daemon()
    }

    pub fn active_name() -> &'static str {
        detect_for_current_dir().name()
    }

    pub fn name(&self) -> &'static str {
        self.backend.name()
    }

    pub fn name_for_project(project: &ProjectConfig) -> &'static str {
        detect_for_project(project).name()
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

    pub fn compose_stream(&self, args: &[&str], passthrough: bool) -> Result<i32> {
        self.backend.compose_stream(&self.ctx(), args, passthrough)
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

    pub fn db_services(&self) -> Result<Vec<DbService>> {
        self.backend.db_services(&self.ctx())
    }

    pub fn project_containers(
        &self,
        include_stopped: bool,
    ) -> Result<Vec<RuntimeProjectContainer>> {
        self.backend
            .project_containers(&self.ctx(), include_stopped)
    }

    pub fn container_ip(&self, container_id: &str) -> Result<String> {
        self.backend.container_ip(container_id)
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

pub fn active_backend() -> Box<dyn ContainerRuntime> {
    detect_for_current_dir()
}

pub fn backend_for_name(runtime: &str) -> Result<Box<dyn ContainerRuntime>> {
    let runtime = normalize_runtime(runtime)?;
    Ok(backend_for(Some(runtime.as_str())))
}

#[cfg(target_os = "macos")]
pub fn known_runtime_names() -> &'static [&'static str] {
    &["apple", "docker"]
}

#[cfg(not(target_os = "macos"))]
pub fn known_runtime_names() -> &'static [&'static str] {
    &["docker"]
}

pub fn container_exec_command(
    runtime: &str,
    container_id: &str,
    env_pairs: &[(String, String)],
    interactive: bool,
    command_args: &[String],
) -> Command {
    let binary = if runtime == "apple" {
        "container"
    } else {
        "docker"
    };
    let mut cmd = Command::new(binary);
    cmd.arg("exec");
    if interactive {
        cmd.arg("-i");
    }
    for (key, value) in env_pairs {
        cmd.arg("-e");
        cmd.arg(format!("{key}={value}"));
    }
    cmd.arg(container_id);
    cmd.args(command_args);
    cmd
}

pub fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

pub fn bench_measurement(
    runtime: &str,
    spec: &RuntimeBenchSpec,
    totals: RuntimeBenchTotals,
) -> RuntimeBenchMeasurement {
    let iterations = spec.iterations.max(1);
    let start_total_ms = duration_ms(totals.start);
    let exec_total_ms = duration_ms(totals.exec);
    let disk_total_ms = duration_ms(totals.disk);
    let total_ms = start_total_ms + exec_total_ms + disk_total_ms;
    let disk_seconds = totals.disk.as_secs_f64();

    RuntimeBenchMeasurement {
        runtime: runtime.to_string(),
        image: spec.image.clone(),
        iterations,
        warmup: spec.warmup,
        start_ms: start_total_ms / iterations as f64,
        start_total_ms,
        exec_ms: exec_total_ms / iterations as f64,
        exec_total_ms,
        disk_ms: disk_total_ms / iterations as f64,
        disk_total_ms,
        disk_mib_per_s: if disk_seconds > 0.0 {
            (spec.size_mb as f64 * 2.0 * iterations as f64) / disk_seconds
        } else {
            0.0
        },
        total_ms,
    }
}

pub fn steady_bench_measurement(
    runtime: &str,
    spec: &RuntimeBenchSpec,
    totals: RuntimeBenchTotals,
) -> RuntimeBenchMeasurement {
    let iterations = spec.iterations.max(1);
    let start_total_ms = duration_ms(totals.start);
    let exec_total_ms = duration_ms(totals.exec);
    let disk_total_ms = duration_ms(totals.disk);
    let total_ms = start_total_ms + exec_total_ms + disk_total_ms;
    let disk_seconds = totals.disk.as_secs_f64();

    RuntimeBenchMeasurement {
        runtime: runtime.to_string(),
        image: spec.image.clone(),
        iterations,
        warmup: spec.warmup,
        start_ms: start_total_ms,
        start_total_ms,
        exec_ms: exec_total_ms / iterations as f64,
        exec_total_ms,
        disk_ms: disk_total_ms / iterations as f64,
        disk_total_ms,
        disk_mib_per_s: if disk_seconds > 0.0 {
            (spec.size_mb as f64 * 2.0 * iterations as f64) / disk_seconds
        } else {
            0.0
        },
        total_ms,
    }
}

pub fn bench_disk_script(path: &str, size_mb: u64) -> String {
    format!(
        "set -eu; \
         p={path:?}; \
         dd if=/dev/zero of=\"$p\" bs=1M count={size_mb} 2>/dev/null; \
         sync; \
         dd if=\"$p\" of=/dev/null bs=1M 2>/dev/null; \
         rm -f \"$p\""
    )
}

// ─── detection ────────────────────────────────────────────────────────────────

/// Auto-detect which runtime is available.
///
/// Linux uses the Docker-compatible backend directly. Runtime selection is a
/// macOS-only feature because Apple Container is only available there.
fn detect() -> Box<dyn ContainerRuntime> {
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(DockerRuntime)
    }

    #[cfg(target_os = "macos")]
    {
        let process_runtime = std::env::var("DIP_RUNTIME").ok();
        if process_runtime
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            return backend_for(process_runtime.as_deref());
        }

        backend_for(read_global_runtime().ok().flatten().as_deref())
    }
}

fn detect_for_current_dir() -> Box<dyn ContainerRuntime> {
    match ProjectConfig::load() {
        Ok(project) => detect_for_project(&project),
        Err(_) => detect(),
    }
}

fn detect_for_project(project: &ProjectConfig) -> Box<dyn ContainerRuntime> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = project;
        Box::new(DockerRuntime)
    }

    #[cfg(target_os = "macos")]
    {
        let process_runtime = std::env::var("DIP_RUNTIME").ok();
        if process_runtime
            .as_deref()
            .is_some_and(|value| !value.is_empty())
        {
            return backend_for(process_runtime.as_deref());
        }

        if let Ok(Some(runtime)) = read_project_runtime(project) {
            return backend_for(Some(runtime.as_str()));
        }

        let project_env = project.get_env();
        if let Some(runtime) = project_env.get("DIP_RUNTIME")
            && !runtime.is_empty()
        {
            return backend_for(Some(runtime.as_str()));
        }

        backend_for(read_global_runtime().ok().flatten().as_deref())
    }
}

#[cfg(target_os = "macos")]
fn backend_for(runtime: Option<&str>) -> Box<dyn ContainerRuntime> {
    if runtime.is_some_and(wants_apple_runtime) {
        Box::new(AppleContainerRuntime)
    } else {
        Box::new(DockerRuntime)
    }
}

#[cfg(not(target_os = "macos"))]
fn backend_for(runtime: Option<&str>) -> Box<dyn ContainerRuntime> {
    let _ = runtime;
    Box::new(DockerRuntime)
}

#[cfg(target_os = "macos")]
fn wants_apple_runtime(runtime: &str) -> bool {
    matches!(
        runtime.to_ascii_lowercase().as_str(),
        "apple" | "container" | "apple-container"
    )
}

#[cfg(target_os = "macos")]
pub fn global_runtime_path() -> PathBuf {
    crate::dirs::config_dir().join("runtime")
}

#[cfg(target_os = "macos")]
pub fn project_runtime_path(project: &ProjectConfig) -> PathBuf {
    project.dip_dir.join("runtime")
}

#[cfg(target_os = "macos")]
pub fn set_global_runtime(runtime: Option<&str>) -> Result<()> {
    let path = global_runtime_path();
    set_runtime_file(path, runtime)
}

#[cfg(target_os = "macos")]
pub fn set_project_runtime(project: &ProjectConfig, runtime: Option<&str>) -> Result<()> {
    set_runtime_file(project_runtime_path(project), runtime)
}

#[cfg(target_os = "macos")]
fn set_runtime_file(path: PathBuf, runtime: Option<&str>) -> Result<()> {
    if let Some(runtime) = runtime {
        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("runtime config path has no parent"))?,
        )?;
        std::fs::write(path, format!("{}\n", normalize_runtime(runtime)?))?;
    } else if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn read_global_runtime() -> Result<Option<String>> {
    read_runtime_file(global_runtime_path())
}

#[cfg(target_os = "macos")]
fn read_project_runtime(project: &ProjectConfig) -> Result<Option<String>> {
    read_runtime_file(project_runtime_path(project))
}

#[cfg(target_os = "macos")]
fn read_runtime_file(path: PathBuf) -> Result<Option<String>> {
    if !path.is_file() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(path)?;
    let runtime = raw.trim();
    if runtime.is_empty() || runtime.eq_ignore_ascii_case("auto") {
        Ok(None)
    } else {
        Ok(Some(normalize_runtime(runtime)?))
    }
}

fn normalize_runtime(runtime: &str) -> Result<String> {
    let runtime = runtime.trim().to_ascii_lowercase();
    match runtime.as_str() {
        #[cfg(target_os = "macos")]
        "apple" | "container" | "apple-container" => Ok("apple".to_string()),
        "docker" => Ok("docker".to_string()),
        "auto" | "" => Ok("auto".to_string()),
        #[cfg(target_os = "macos")]
        _ => anyhow::bail!("unsupported runtime '{runtime}', expected apple, docker, or auto"),
        #[cfg(not(target_os = "macos"))]
        _ => anyhow::bail!("unsupported runtime '{runtime}', expected docker or auto"),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize_runtime;

    #[cfg(target_os = "macos")]
    #[test]
    fn detects_apple_runtime_aliases() {
        use super::wants_apple_runtime;

        assert!(wants_apple_runtime("apple"));
        assert!(wants_apple_runtime("container"));
        assert!(wants_apple_runtime("apple-container"));
        assert!(!wants_apple_runtime("docker"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn normalizes_runtime_names() {
        assert_eq!(normalize_runtime("container").unwrap(), "apple");
        assert_eq!(normalize_runtime("docker").unwrap(), "docker");
        assert!(normalize_runtime("nope").is_err());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn normalizes_only_docker_on_non_macos() {
        assert_eq!(normalize_runtime("docker").unwrap(), "docker");
        assert_eq!(normalize_runtime("auto").unwrap(), "auto");
        assert!(normalize_runtime("apple").is_err());
    }
}
