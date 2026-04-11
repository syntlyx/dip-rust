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

    if verbose {
        eprintln!("  docker inspect {}", ids.join(" "));
    }

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
        let env = parse_container_env(&c["Config"]["Env"]);

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

fn parse_container_env(env_array: &Value) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if let Some(arr) = env_array.as_array() {
        for entry in arr {
            if let Some(s) = entry.as_str()
                && let Some((k, v)) = s.split_once('=')
            {
                map.insert(k.to_string(), v.to_string());
            }
        }
    }
    map
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
