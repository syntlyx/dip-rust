use std::path::Path;

use anyhow::Result;

use super::{DbBackend, DbConfig, exec_dump, exec_import, is_gzipped};
use crate::runtime::container_exec_command;
use crate::utils::output::Output;

pub struct MySqlBackend;

impl DbBackend for MySqlBackend {
    fn name(&self) -> &str {
        "mysql"
    }

    fn console_cmd(&self, config: &DbConfig) -> Vec<String> {
        // Password is NOT passed here — it comes via console_env() as MYSQL_PWD.
        vec![
            "mysql".into(),
            "-u".into(),
            config.user.clone(),
            config.db_name.clone(),
        ]
    }

    // MYSQL_PWD is passed via `docker exec -e`, so the password never appears in
    // command-line arguments (visible in `ps aux` / `/proc/*/cmdline`).
    fn console_env(&self, config: &DbConfig) -> Vec<(String, String)> {
        vec![("MYSQL_PWD".to_string(), config.password.clone())]
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
            "Exporting MySQL database '{}' → {}",
            config.db_name,
            output_path.display()
        ));

        let gz = is_gzipped(output_path);
        let env_pairs = vec![("MYSQL_PWD".to_string(), config.password.clone())];
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
                        "mysqldump -u\"$1\" \"$2\" | gzip".to_string(),
                        "sh".to_string(),
                        user,
                        db,
                    ]
                } else {
                    vec!["mysqldump".to_string(), format!("-u{user}"), db]
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
            "Importing into MySQL database '{}'...",
            config.db_name
        ));

        let env_pairs = vec![("MYSQL_PWD".to_string(), config.password.clone())];
        let runtime = runtime.to_string();
        let cid = container_id.to_string();
        let user = config.user.clone();
        let db = config.db_name.clone();

        exec_import(
            input_path,
            move |gz| {
                let setup = "SET SESSION autocommit=0; SET SESSION unique_checks=0; \
                             SET SESSION foreign_key_checks=0; SET SESSION sql_log_bin=0;"
                    .to_string();
                let command_args = if gz {
                    vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        "{ printf '%s\\n' \"$3\"; gunzip -c; printf '\\nCOMMIT;\\n'; } | mysql -u\"$1\" \"$2\"".to_string(),
                        "sh".to_string(),
                        user,
                        db,
                        setup,
                    ]
                } else {
                    vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        "{ printf '%s\\n' \"$3\"; cat; printf '\\nCOMMIT;\\n'; } | mysql -u\"$1\" \"$2\"".to_string(),
                        "sh".to_string(),
                        user,
                        db,
                        setup,
                    ]
                };
                container_exec_command(&runtime, &cid, &env_pairs, true, &command_args)
            },
            out,
        )
    }
}
