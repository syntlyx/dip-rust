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
    env_vars: HashMap<String, String>,
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

        Ok(Self {
            root_dir,
            dip_dir,
            env_file,
            compose_file,
            project_name,
            env_vars,
        })
    }

    /// Merge additional variables into the project environment.
    /// Called after running hooks so hook output (e.g. AWS credentials) flows
    /// into every subsequent docker-compose invocation.
    pub fn merge_env(&mut self, vars: HashMap<String, String>) {
        self.env_vars.extend(vars);
    }

    /// Build the full environment for docker-compose, merging system env + .env overrides
    pub fn get_env(&self) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = std::env::vars().collect();
        env.extend(self.env_vars.clone());

        let (uid, gid) = get_uid_gid();

        env.insert(
            config::ENV_PROJECT_ROOT.to_string(),
            self.root_dir.to_string_lossy().into_owned(),
        );
        env.insert(
            config::ENV_PROJECT_NAME.to_string(),
            self.project_name.clone(),
        );
        env.insert(
            config::ENV_COMPOSE_NAME.to_string(),
            self.project_name.clone(),
        );
        env.insert(
            config::ENV_DIP_DIR.to_string(),
            self.dip_dir.to_string_lossy().into_owned(),
        );
        env.insert(
            config::ENV_FILE.to_string(),
            self.env_file.to_string_lossy().into_owned(),
        );
        env.insert(config::ENV_HOST_UID.to_string(), uid);
        env.insert(config::ENV_HOST_GID.to_string(), gid);

        env
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
    // On macOS, Docker runs inside a Linux VM with its own filesystem layer
    // (OrbStack / Docker Desktop). UID/GID matching between host and container
    // is not needed — the VM handles permissions transparently.
    // Using 1000:1000 avoids conflicts with macOS system GIDs (e.g. GID 20 = staff
    // already exists in most Linux base images as "dialout").
    #[cfg(target_os = "macos")]
    return ("1000".to_string(), "1000".to_string());

    #[cfg(not(target_os = "macos"))]
    {
        // On Linux, bind-mounted files are shared directly with the host kernel.
        // UID/GID must match so the container user can read/write mounted files.
        let uid = std::process::Command::new("id")
            .arg("-u")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "1000".to_string());

        let gid = std::process::Command::new("id")
            .arg("-g")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_else(|_| "1000".to_string());

        (uid, gid)
    }
}
