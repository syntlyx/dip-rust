pub mod migrate;
pub mod mysql;
pub mod postgres;

pub use mysql::MySqlBackend;
pub use postgres::PostgresBackend;

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use anyhow::Result;

use crate::project::ProjectConfig;
use crate::runtime::Runtime;
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
    /// Returns the argv for opening an interactive DB console inside the container.
    fn console_cmd(&self, config: &DbConfig) -> Vec<String>;
    /// Extra environment variables to pass via `docker exec -e` when opening a console.
    /// Backends use this to pass credentials without leaking them in command-line args.
    fn console_env(&self, _config: &DbConfig) -> Vec<(String, String)> {
        vec![]
    }
    fn dump(
        &self,
        runtime: &str,
        container_id: &str,
        config: &DbConfig,
        output_path: &Path,
        out: &Output,
    ) -> Result<()>;
    fn import(
        &self,
        runtime: &str,
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
/// Credentials are read directly from the container's environment, not from .env.
pub fn detect_by_labels(project: &ProjectConfig, verbose: bool) -> Result<Vec<DbService>> {
    Runtime::new(project.clone(), verbose, true).db_services()
}

// ─── shared execution helpers ────────────────────────────────────────────────

/// Run a dump command with stdout redirected to `output_path`.
///
/// `build_cmd` should return a fully-configured `Command` (without stdout —
/// that is set here). The output file is removed on failure.
pub fn exec_dump(
    output_path: &Path,
    build_cmd: impl FnOnce() -> std::process::Command,
    out: &Output,
) -> Result<()> {
    let output_file = create_output_file(output_path)?;
    let status = build_cmd().stdout(output_file).status()?;
    if status.success() {
        out.success(&format!("Database exported to {}", output_path.display()));
        Ok(())
    } else {
        let _ = std::fs::remove_file(output_path);
        anyhow::bail!(
            "Database dump failed — check that the container is running and credentials are correct"
        )
    }
}

/// Copy dump file to container, run import command, clean up temp file.
///
/// `build_cmd` receives the compression flag and returns a command that reads
/// SQL from stdin. This avoids runtime-specific `docker cp` / `container cp`
/// behavior and works for both Docker and Apple Container.
pub fn exec_import(
    input_path: &Path,
    build_cmd: impl FnOnce(bool) -> std::process::Command,
    out: &Output,
) -> Result<()> {
    let gz = is_gzipped(input_path);
    let input = File::open(input_path)
        .map_err(|e| anyhow::anyhow!("Cannot open import file {}: {e}", input_path.display()))?;

    out.info("Streaming dump file into container...");

    let import_status = build_cmd(gz).stdin(input).status()?;

    if import_status.success() {
        out.success("Database imported successfully");
        Ok(())
    } else {
        anyhow::bail!("Database import failed")
    }
}

// ─── compression helpers ──────────────────────────────────────────────────────

pub fn is_gzipped(path: &Path) -> bool {
    path.to_str().map(|s| s.ends_with(".gz")).unwrap_or(false)
}

/// Create the local output file for a dump, returning an error with the path
/// in the message if it fails.
pub fn create_output_file(path: &Path) -> Result<std::fs::File> {
    std::fs::File::create(path)
        .map_err(|e| anyhow::anyhow!("Cannot create output file {}: {e}", path.display()))
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
