use std::path::Path;

use anyhow::Result;

use super::{DbBackend, DbConfig};
use crate::utils::output::Output;

pub struct MySqlBackend;

impl DbBackend for MySqlBackend {
    fn name(&self) -> &str {
        "mysql"
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

        let output_file = std::fs::File::create(output_path).map_err(|e| {
            anyhow::anyhow!("Cannot create output file {}: {e}", output_path.display())
        })?;

        let status = std::process::Command::new("docker")
            .args([
                "exec",
                container_id,
                "mysqldump",
                "-uroot",
                &format!("-p{}", config.password),
                &config.db_name,
            ])
            .stdout(output_file)
            .status()?;

        if status.success() {
            out.success(&format!("Database exported to {}", output_path.display()));
            Ok(())
        } else {
            let _ = std::fs::remove_file(output_path);
            anyhow::bail!(
                "mysqldump failed — check that the db container is running and credentials are correct"
            )
        }
    }

    fn import(
        &self,
        container_id: &str,
        config: &DbConfig,
        input_path: &Path,
        out: &Output,
    ) -> Result<()> {
        out.info("Copying dump file to container...");
        let cp_status = std::process::Command::new("docker")
            .args([
                "cp",
                input_path.to_str().unwrap(),
                &format!("{container_id}:/tmp/import.sql"),
            ])
            .status()?;
        if !cp_status.success() {
            anyhow::bail!("Failed to copy dump file to container");
        }

        out.info(&format!(
            "Importing into MySQL database '{}'...",
            config.db_name
        ));
        let import_sql = format!(
            "mysql -uroot -p{} {} \
             -e 'SET SESSION autocommit=0; SET SESSION unique_checks=0; \
             SET SESSION foreign_key_checks=0; SET SESSION sql_log_bin=0;' \
             -e 'SOURCE /tmp/import.sql;' \
             -e 'COMMIT;'",
            config.password, config.db_name
        );
        let import_status = std::process::Command::new("docker")
            .args(["exec", container_id, "sh", "-c", &import_sql])
            .status()?;

        let _ = std::process::Command::new("docker")
            .args(["exec", container_id, "rm", "-f", "/tmp/import.sql"])
            .status();

        if import_status.success() {
            out.success("Database imported successfully");
            Ok(())
        } else {
            anyhow::bail!("Database import failed")
        }
    }
}
