use std::path::Path;

use anyhow::Result;

use super::{DbBackend, DbConfig, exec_dump, exec_import, is_gzipped};
use crate::utils::output::Output;

pub struct PostgresBackend;

impl DbBackend for PostgresBackend {
    fn name(&self) -> &str {
        "postgres"
    }

    fn console_cmd(&self, config: &DbConfig) -> Vec<String> {
        vec![
            "psql".into(),
            "-U".into(),
            config.user.clone(),
            "-d".into(),
            config.db_name.clone(),
        ]
    }

    fn console_env(&self, config: &DbConfig) -> Vec<(String, String)> {
        vec![("PGPASSWORD".to_string(), config.password.clone())]
    }

    fn dump(
        &self,
        container_id: &str,
        config: &DbConfig,
        output_path: &Path,
        out: &Output,
    ) -> Result<()> {
        out.info(&format!(
            "Exporting PostgreSQL database '{}' → {}",
            config.db_name,
            output_path.display()
        ));

        let gz = is_gzipped(output_path);
        let pg_env = format!("PGPASSWORD={}", config.password);
        let cid = container_id.to_string();
        let user = config.user.clone();
        let db = config.db_name.clone();

        exec_dump(
            output_path,
            move || {
                let mut cmd = std::process::Command::new("docker");
                if gz {
                    // Pipe requires sh -c; password is in -e (not in the shell string).
                    let shell = format!("pg_dump -U {user} {db} | gzip");
                    cmd.args(["exec", "-e", &pg_env, &cid, "sh", "-c", &shell]);
                } else {
                    cmd.args(["exec", "-e", &pg_env, &cid, "pg_dump", "-U", &user, &db]);
                }
                cmd
            },
            out,
        )
    }

    fn import(
        &self,
        container_id: &str,
        config: &DbConfig,
        input_path: &Path,
        out: &Output,
    ) -> Result<()> {
        out.info(&format!(
            "Importing into PostgreSQL database '{}'...",
            config.db_name
        ));

        let pg_env = format!("PGPASSWORD={}", config.password);
        let cid = container_id.to_string();
        let user = config.user.clone();
        let db = config.db_name.clone();

        exec_import(
            container_id,
            input_path,
            move |gz, remote| {
                let mut cmd = std::process::Command::new("docker");
                if gz {
                    // Pipe requires sh -c; password is in -e (not in the shell string).
                    let shell = format!("gunzip -c {remote} | psql -U {user} -d {db}");
                    cmd.args(["exec", "-e", &pg_env, &cid, "sh", "-c", &shell]);
                } else {
                    cmd.args([
                        "exec", "-e", &pg_env, &cid, "psql", "-U", &user, "-d", &db, "-f", remote,
                    ]);
                }
                cmd
            },
            out,
        )
    }
}
