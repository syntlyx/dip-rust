use std::path::Path;

use anyhow::Result;

use super::{DbBackend, DbConfig, exec_dump, exec_import, is_gzipped};
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
        let mysql_env = format!("MYSQL_PWD={}", config.password);
        let cid = container_id.to_string();
        let user = config.user.clone();
        let db = config.db_name.clone();

        exec_dump(
            output_path,
            move || {
                let mut cmd = std::process::Command::new("docker");
                if gz {
                    // Pipe requires sh -c; password is in -e (not in the shell string).
                    let shell = format!("mysqldump -u{user} {db} | gzip");
                    cmd.args(["exec", "-e", &mysql_env, &cid, "sh", "-c", &shell]);
                } else {
                    cmd.args([
                        "exec",
                        "-e",
                        &mysql_env,
                        &cid,
                        "mysqldump",
                        &format!("-u{user}"),
                        &db,
                    ]);
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
            "Importing into MySQL database '{}'...",
            config.db_name
        ));

        let mysql_env = format!("MYSQL_PWD={}", config.password);
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
                    let shell = format!("gunzip -c {remote} | mysql -u{user} {db}");
                    cmd.args(["exec", "-e", &mysql_env, &cid, "sh", "-c", &shell]);
                } else {
                    // Direct argv with multiple -e statements — no sh -c needed.
                    cmd.args([
                        "exec",
                        "-e",
                        &mysql_env,
                        &cid,
                        "mysql",
                        &format!("-u{user}"),
                        &db,
                        "-e",
                        "SET SESSION autocommit=0; SET SESSION unique_checks=0; \
                               SET SESSION foreign_key_checks=0; SET SESSION sql_log_bin=0;",
                        "-e",
                        &format!("SOURCE {remote};"),
                        "-e",
                        "COMMIT;",
                    ]);
                }
                cmd
            },
            out,
        )
    }
}
