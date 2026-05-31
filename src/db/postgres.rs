use std::path::Path;

use anyhow::Result;

use super::{DbBackend, DbConfig, exec_dump, exec_import, is_gzipped};
use crate::runtime::container_exec_command;
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
        runtime: &str,
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
        let env_pairs = vec![("PGPASSWORD".to_string(), config.password.clone())];
        let runtime = runtime.to_string();
        let cid = container_id.to_string();
        let user = config.user.clone();
        let db = config.db_name.clone();

        exec_dump(
            output_path,
            move || {
                let command_args = if gz {
                    // Pipe requires sh -c; password is in -e (not in the shell string).
                    vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        "pg_dump -U \"$1\" \"$2\" | gzip".to_string(),
                        "sh".to_string(),
                        user,
                        db,
                    ]
                } else {
                    vec!["pg_dump".to_string(), "-U".to_string(), user, db]
                };
                container_exec_command(&runtime, &cid, &env_pairs, false, &command_args)
            },
            out,
        )
    }

    fn import(
        &self,
        runtime: &str,
        container_id: &str,
        config: &DbConfig,
        input_path: &Path,
        out: &Output,
    ) -> Result<()> {
        out.info(&format!(
            "Importing into PostgreSQL database '{}'...",
            config.db_name
        ));

        let env_pairs = vec![("PGPASSWORD".to_string(), config.password.clone())];
        let runtime = runtime.to_string();
        let cid = container_id.to_string();
        let user = config.user.clone();
        let db = config.db_name.clone();

        exec_import(
            input_path,
            move |gz| {
                let command_args = if gz {
                    // Pipe requires sh -c; password is in -e (not in the shell string).
                    vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        "gunzip -c | psql -U \"$1\" -d \"$2\"".to_string(),
                        "sh".to_string(),
                        user,
                        db,
                    ]
                } else {
                    vec![
                        "psql".to_string(),
                        "-U".to_string(),
                        user,
                        "-d".to_string(),
                        db,
                    ]
                };
                container_exec_command(&runtime, &cid, &env_pairs, true, &command_args)
            },
            out,
        )
    }
}
