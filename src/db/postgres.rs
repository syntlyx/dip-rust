use std::path::Path;

use anyhow::Result;

use super::{
    DbBackend, DbConfig, create_output_file, docker_cp_to_container, docker_rm_remote, is_gzipped,
    remote_tmp_path,
};
use crate::utils::output::Output;

pub struct PostgresBackend;

impl DbBackend for PostgresBackend {
    fn name(&self) -> &str {
        "postgres"
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

        let output_file = create_output_file(output_path)?;

        let status = if is_gzipped(output_path) {
            std::process::Command::new("docker")
                .args([
                    "exec",
                    "-e",
                    &format!("PGPASSWORD={}", config.password),
                    container_id,
                    "sh",
                    "-c",
                    &format!("pg_dump -U {} {} | gzip", config.user, config.db_name),
                ])
                .stdout(output_file)
                .status()?
        } else {
            std::process::Command::new("docker")
                .args([
                    "exec",
                    "-e",
                    &format!("PGPASSWORD={}", config.password),
                    container_id,
                    "pg_dump",
                    "-U",
                    &config.user,
                    &config.db_name,
                ])
                .stdout(output_file)
                .status()?
        };

        if status.success() {
            out.success(&format!("Database exported to {}", output_path.display()));
            Ok(())
        } else {
            let _ = std::fs::remove_file(output_path);
            anyhow::bail!(
                "pg_dump failed — check that the db container is running and credentials are correct"
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
        let gz = is_gzipped(input_path);
        let remote = remote_tmp_path(gz);

        out.info("Copying dump file to container...");
        docker_cp_to_container(container_id, input_path, remote)?;

        out.info(&format!(
            "Importing into PostgreSQL database '{}'...",
            config.db_name
        ));

        let import_status = if gz {
            let import_cmd = format!(
                "gunzip -c {remote} | PGPASSWORD={} psql -U {} -d {}",
                config.password, config.user, config.db_name
            );
            std::process::Command::new("docker")
                .args(["exec", container_id, "sh", "-c", &import_cmd])
                .status()?
        } else {
            std::process::Command::new("docker")
                .args([
                    "exec",
                    "-e",
                    &format!("PGPASSWORD={}", config.password),
                    container_id,
                    "psql",
                    "-U",
                    &config.user,
                    "-d",
                    &config.db_name,
                    "-f",
                    remote,
                ])
                .status()?
        };

        docker_rm_remote(container_id, remote);

        if import_status.success() {
            out.success("Database imported successfully");
            Ok(())
        } else {
            anyhow::bail!("Database import failed")
        }
    }
}
