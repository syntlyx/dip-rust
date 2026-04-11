pub mod mysql;
pub mod postgres;

pub use mysql::MySqlBackend;
pub use postgres::PostgresBackend;

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::Result;
use serde_json::Value;

use crate::project::ProjectConfig;
use crate::utils::env;
use crate::utils::output::Output;

// ─── config ──────────────────────────────────────────────────────────────────

pub struct DbConfig {
    pub db_name: String,
    pub password: String,
    pub user: String,
}

// ─── trait ───────────────────────────────────────────────────────────────────

pub trait DbBackend {
    fn name(&self) -> &str;
    fn dump(
        &self,
        container_id: &str,
        config: &DbConfig,
        output_path: &Path,
        out: &Output,
    ) -> Result<()>;
    fn import(
        &self,
        container_id: &str,
        config: &DbConfig,
        input_path: &Path,
        out: &Output,
    ) -> Result<()>;
}

// ─── label-based DB service ───────────────────────────────────────────────────

pub struct DbService {
    pub service_name: String,
    pub container_id: String,
    pub backend: Box<dyn DbBackend>,
    pub config: DbConfig,
}

/// Inspect all running compose containers and return those tagged with `dip.db` label.
/// Credentials are read directly from the container's environment (docker inspect), not from .env.
pub fn detect_by_labels(project: &ProjectConfig, verbose: bool) -> Result<Vec<DbService>> {
    let compose_file = project.compose_file.to_string_lossy().into_owned();

    // Get IDs of all running compose services
    let ps_out = Command::new("docker")
        .args(["compose", "-f", &compose_file, "ps", "-q"])
        .envs(project.get_env())
        .output()?;

    let ids: Vec<&str> = std::str::from_utf8(&ps_out.stdout)?
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if ids.is_empty() {
        return Ok(vec![]);
    }

    crate::utils::log_verbose(verbose, &format!("  docker inspect {}", ids.join(" ")));

    let mut inspect_args = vec!["inspect"];
    inspect_args.extend_from_slice(&ids);

    let inspect_out = Command::new("docker").args(&inspect_args).output()?;
    if !inspect_out.status.success() {
        return Ok(vec![]);
    }

    let containers: Value = serde_json::from_slice(&inspect_out.stdout)?;
    let containers = match containers.as_array() {
        Some(a) => a,
        None => return Ok(vec![]),
    };

    let mut services = vec![];

    for c in containers {
        let labels = &c["Config"]["Labels"];
        let db_type = match labels["dip.db"].as_str() {
            Some(t) => t,
            None => continue,
        };

        let container_id = match c["Id"].as_str() {
            Some(id) => id[..12].to_string(),
            None => continue,
        };

        // Service name from compose label, fall back to container Name
        let service_name = labels["com.docker.compose.service"]
            .as_str()
            .unwrap_or_else(|| c["Name"].as_str().unwrap_or("db").trim_start_matches('/'))
            .to_string();

        // Parse env array ["KEY=VALUE", ...] into a map
        let env = env::parse_env_json_array(&c["Config"]["Env"]);

        let (backend, config) = match db_type {
            "mysql" => {
                let db_name = env.get("MYSQL_DATABASE").cloned().unwrap_or_default();
                let password = env.get("MYSQL_ROOT_PASSWORD").cloned().unwrap_or_default();
                if db_name.is_empty() || password.is_empty() {
                    continue; // label present but creds missing — skip
                }
                let b: Box<dyn DbBackend> = Box::new(MySqlBackend);
                (
                    b,
                    DbConfig {
                        db_name,
                        password,
                        user: "root".to_string(),
                    },
                )
            }
            "postgres" => {
                let db_name = env
                    .get("POSTGRES_DB")
                    .or_else(|| env.get("PGDATABASE"))
                    .cloned()
                    .unwrap_or_default();
                let password = env
                    .get("POSTGRES_PASSWORD")
                    .or_else(|| env.get("PGPASSWORD"))
                    .cloned()
                    .unwrap_or_default();
                if db_name.is_empty() || password.is_empty() {
                    continue;
                }
                let user = env
                    .get("POSTGRES_USER")
                    .or_else(|| env.get("PGUSER"))
                    .cloned()
                    .unwrap_or_else(|| "postgres".to_string());
                let b: Box<dyn DbBackend> = Box::new(PostgresBackend);
                (
                    b,
                    DbConfig {
                        db_name,
                        password,
                        user,
                    },
                )
            }
            _ => continue,
        };

        services.push(DbService {
            service_name,
            container_id,
            backend,
            config,
        });
    }

    Ok(services)
}

// ─── compression helpers ──────────────────────────────────────────────────────

pub fn is_gzipped(path: &Path) -> bool {
    path.to_str().map(|s| s.ends_with(".gz")).unwrap_or(false)
}

/// Returns the remote tmp path to use when copying a dump into a container.
pub fn remote_tmp_path(gz: bool) -> &'static str {
    if gz {
        "/tmp/import.sql.gz"
    } else {
        "/tmp/import.sql"
    }
}

// ─── shared docker helpers ────────────────────────────────────────────────────

/// Create the local output file for a dump, returning an error with the path
/// in the message if it fails.
pub fn create_output_file(path: &Path) -> Result<std::fs::File> {
    std::fs::File::create(path)
        .map_err(|e| anyhow::anyhow!("Cannot create output file {}: {e}", path.display()))
}

/// Copy a local file into a container via `docker cp`.
pub fn docker_cp_to_container(container_id: &str, local: &Path, remote: &str) -> Result<()> {
    let status = std::process::Command::new("docker")
        .args([
            "cp",
            local.to_str().unwrap_or_default(),
            &format!("{container_id}:{remote}"),
        ])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("Failed to copy dump file to container")
    }
}

/// Remove a file inside a container (best-effort, errors are silently ignored).
pub fn docker_rm_remote(container_id: &str, remote: &str) {
    let _ = std::process::Command::new("docker")
        .args(["exec", container_id, "rm", "-f", remote])
        .status();
}

// ─── legacy env-based detection ──────────────────────────────────────────────

/// Detect which database backend is configured from .env variables.
/// MySQL is checked first (MYSQL_DATABASE + MYSQL_ROOT_PASSWORD).
/// PostgreSQL is checked second (POSTGRES_DB + POSTGRES_PASSWORD, or PG* aliases).
pub fn detect(env: &HashMap<String, String>) -> Result<(Box<dyn DbBackend>, DbConfig)> {
    if let (Some(db_name), Some(password)) =
        (env.get("MYSQL_DATABASE"), env.get("MYSQL_ROOT_PASSWORD"))
    {
        return Ok((
            Box::new(MySqlBackend),
            DbConfig {
                db_name: db_name.clone(),
                password: password.clone(),
                user: "root".to_string(),
            },
        ));
    }

    if let (Some(db_name), Some(password)) = (
        env.get("POSTGRES_DB").or_else(|| env.get("PGDATABASE")),
        env.get("POSTGRES_PASSWORD")
            .or_else(|| env.get("PGPASSWORD")),
    ) {
        let user = env
            .get("POSTGRES_USER")
            .or_else(|| env.get("PGUSER"))
            .cloned()
            .unwrap_or_else(|| "postgres".to_string());
        return Ok((
            Box::new(PostgresBackend),
            DbConfig {
                db_name: db_name.clone(),
                password: password.clone(),
                user,
            },
        ));
    }

    anyhow::bail!(
        "No database credentials found in .env\n  \
         MySQL:      set MYSQL_DATABASE + MYSQL_ROOT_PASSWORD\n  \
         PostgreSQL: set POSTGRES_DB + POSTGRES_PASSWORD"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── detect() ──────────────────────────────────────────────────────────────

    #[test]
    fn detects_mysql() {
        let env = HashMap::from([
            ("MYSQL_DATABASE".to_string(), "mydb".to_string()),
            ("MYSQL_ROOT_PASSWORD".to_string(), "secret".to_string()),
        ]);
        let (backend, cfg) = detect(&env).unwrap();
        assert_eq!(backend.name(), "mysql");
        assert_eq!(cfg.db_name, "mydb");
        assert_eq!(cfg.user, "root");
    }

    #[test]
    fn detects_postgres_primary_vars() {
        let env = HashMap::from([
            ("POSTGRES_DB".to_string(), "pgdb".to_string()),
            ("POSTGRES_PASSWORD".to_string(), "pgpass".to_string()),
            ("POSTGRES_USER".to_string(), "pguser".to_string()),
        ]);
        let (backend, cfg) = detect(&env).unwrap();
        assert_eq!(backend.name(), "postgres");
        assert_eq!(cfg.db_name, "pgdb");
        assert_eq!(cfg.user, "pguser");
    }

    #[test]
    fn detects_postgres_pg_aliases() {
        let env = HashMap::from([
            ("PGDATABASE".to_string(), "pgdb".to_string()),
            ("PGPASSWORD".to_string(), "pgpass".to_string()),
        ]);
        let (backend, cfg) = detect(&env).unwrap();
        assert_eq!(backend.name(), "postgres");
        assert_eq!(cfg.user, "postgres"); // default user
    }

    #[test]
    fn mysql_takes_priority_over_postgres() {
        let env = HashMap::from([
            ("MYSQL_DATABASE".to_string(), "mydb".to_string()),
            ("MYSQL_ROOT_PASSWORD".to_string(), "secret".to_string()),
            ("POSTGRES_DB".to_string(), "pgdb".to_string()),
            ("POSTGRES_PASSWORD".to_string(), "pgpass".to_string()),
        ]);
        let (backend, _) = detect(&env).unwrap();
        assert_eq!(backend.name(), "mysql");
    }

    #[test]
    fn empty_env_returns_error() {
        let env = HashMap::new();
        assert!(detect(&env).is_err());
    }
}
