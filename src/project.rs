use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::config;
use crate::utils::env;

#[derive(Clone)]
pub struct ProjectConfig {
    pub root_dir: PathBuf,
    pub dip_dir: PathBuf,
    pub env_file: PathBuf,
    pub compose_file: PathBuf,
    pub project_name: String,
    // Pre-built merged env: system env + .env vars + dip constants (uid/gid, paths).
    // Built once at load() so get_env() is a cheap clone with no subprocess calls.
    env: HashMap<String, String>,
}

impl ProjectConfig {
    pub fn load() -> Result<Self> {
        let root_dir = find_project_root()
            .ok_or_else(|| anyhow::anyhow!("Not a dip project: '.dip' directory not found"))?;

        let dip_dir = root_dir.join(format!(".{}", config::BIN_NAME));
        let env_file = dip_dir.join(".env");
        let default_env_file = dip_dir.join("default.env");
        let compose_file = dip_dir.join("docker-compose.yml");

        // Create .env from default.env if it doesn't exist yet
        if !env_file.exists() {
            if default_env_file.exists() {
                std::fs::copy(&default_env_file, &env_file)
                    .context("Failed to create .env from default.env")?;
            } else {
                anyhow::bail!("Env file not found: {}", env_file.display());
            }
        }

        let env_vars = env::parse_env_file(&env_file)?;

        let project_name = env_vars
            .get(config::ENV_PROJECT_NAME)
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "Env '{}' must be set in {}",
                    config::ENV_PROJECT_NAME,
                    env_file.display()
                )
            })?;

        // uid/gid: on Linux this spawns `id -u` / `id -g`, so do it once here.
        let (uid, gid) = get_uid_gid();

        let mut env: HashMap<String, String> = std::env::vars().collect();
        env.extend(env_vars);
        env.insert(
            config::ENV_PROJECT_ROOT.to_string(),
            root_dir.to_string_lossy().into_owned(),
        );
        env.insert(config::ENV_PROJECT_NAME.to_string(), project_name.clone());
        env.insert(config::ENV_COMPOSE_NAME.to_string(), project_name.clone());
        env.insert(
            config::ENV_DIP_DIR.to_string(),
            dip_dir.to_string_lossy().into_owned(),
        );
        env.insert(
            config::ENV_FILE.to_string(),
            env_file.to_string_lossy().into_owned(),
        );
        env.insert(config::ENV_HOST_UID.to_string(), uid);
        env.insert(config::ENV_HOST_GID.to_string(), gid);

        Ok(Self {
            root_dir,
            dip_dir,
            env_file,
            compose_file,
            project_name,
            env,
        })
    }

    /// Merge additional variables into the project environment.
    /// Called after running hooks so hook output (e.g. AWS credentials) flows
    /// into every subsequent docker-compose invocation.
    pub fn merge_env(&mut self, vars: HashMap<String, String>) {
        self.env.extend(vars);
    }

    /// Returns the full environment for docker-compose invocations.
    pub fn get_env(&self) -> HashMap<String, String> {
        self.env.clone()
    }
}

fn find_project_root() -> Option<PathBuf> {
    let dir_name = format!(".{}", config::BIN_NAME);
    let mut current = std::env::current_dir().ok()?;
    loop {
        if current.join(&dir_name).is_dir() {
            return Some(current);
        }
        let parent = current.parent()?.to_path_buf();
        if parent == current {
            return None;
        }
        current = parent;
    }
}

fn get_uid_gid() -> (String, String) {
    // macOS: Docker runs in a Linux VM, UID/GID matching not needed.
    // 1000:1000 avoids GID conflicts (e.g. macOS GID 20 = "staff" → "dialout" in Linux images).
    if cfg!(target_os = "macos") {
        return ("1000".to_string(), "1000".to_string());
    }
    // Linux: bind-mounts share the host kernel's filesystem — UID/GID must match.
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    (uid.to_string(), gid.to_string())
}
