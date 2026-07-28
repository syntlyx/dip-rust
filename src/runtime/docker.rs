use std::io::BufRead;
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde_json::Value;

use crate::db::{DbConfig, DbService, MySqlBackend, PostgresBackend};
use crate::utils::env;
use crate::utils::log_verbose;
use crate::utils::output::{Output, colorize_compose_line, make_spinner};

use super::{
    BackendCtx, ContainerRuntime, RuntimeBenchMeasurement, RuntimeBenchSpec, RuntimeBenchTotals,
    RuntimeContainerCounts, RuntimeProjectContainer, RuntimeSystemInfo, bench_disk_script,
    bench_measurement, exit_code, steady_bench_measurement,
};

pub struct DockerRuntime;

impl ContainerRuntime for DockerRuntime {
    fn name(&self) -> &'static str {
        "docker"
    }

    fn check_daemon(&self) -> Result<()> {
        let ok = Command::new("docker")
            .args(["info"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            anyhow::bail!("Docker daemon is not running. Start Docker and try again.")
        }
    }

    fn compose_run(&self, ctx: &BackendCtx, args: &[&str], msg: &str) -> Result<()> {
        let compose_file = ctx.project.compose_file.to_string_lossy().into_owned();
        let mut cmd_args = vec!["compose", "-f", compose_file.as_str()];
        cmd_args.extend_from_slice(args);

        log_cmd(ctx.verbose, &cmd_args);

        let pb = make_spinner(msg);

        let mut child = Command::new("docker")
            .args(&cmd_args)
            .envs(ctx.project.get_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("stdout not captured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow::anyhow!("stderr not captured"))?;

        let pb_out = pb.clone();
        let nc = ctx.no_color;
        let stdout_thread = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stdout)
                .lines()
                .map_while(Result::ok)
            {
                pb_out.println(colorize_compose_line(&line, nc));
            }
        });

        let pb_err = pb.clone();
        let stderr_thread = std::thread::spawn(move || {
            for line in std::io::BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
            {
                if !is_env_warning(&line) {
                    pb_err.println(colorize_compose_line(&line, nc));
                }
            }
        });

        let status = child.wait()?;
        if stdout_thread.join().is_err() {
            log_verbose(ctx.verbose, "  [warn] stdout reader thread panicked");
        }
        if stderr_thread.join().is_err() {
            log_verbose(ctx.verbose, "  [warn] stderr reader thread panicked");
        }
        pb.finish_and_clear();

        if !status.success() {
            anyhow::bail!("docker compose command failed");
        }
        Ok(())
    }

    fn compose_stream(&self, ctx: &BackendCtx, args: &[&str], passthrough: bool) -> Result<i32> {
        let compose_file = ctx.project.compose_file.to_string_lossy().into_owned();
        let mut cmd_args = vec!["compose", "-f", compose_file.as_str()];
        cmd_args.extend_from_slice(args);

        log_cmd(ctx.verbose, &cmd_args);

        let status = Command::new("docker")
            .args(&cmd_args)
            .envs(ctx.project.get_env())
            .status()?;

        let code = exit_code(&status);
        if !status.success() && code != 130 && !passthrough {
            anyhow::bail!("docker compose command failed (exit {})", status);
        }
        Ok(code)
    }

    fn compose_stream_grep(
        &self,
        ctx: &BackendCtx,
        args: &[&str],
        keywords: &[&str],
    ) -> Result<()> {
        use std::io::BufRead;

        let compose_file = ctx.project.compose_file.to_string_lossy().into_owned();
        let mut cmd_args = vec!["compose", "-f", compose_file.as_str()];
        cmd_args.extend_from_slice(args);

        log_cmd(ctx.verbose, &cmd_args);

        let mut child = Command::new("docker")
            .args(&cmd_args)
            .envs(ctx.project.get_env())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("stdout not captured"))?;
        let nc = ctx.no_color;

        for line in std::io::BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
        {
            let lower = line.to_lowercase();
            if keywords.iter().any(|kw| lower.contains(kw)) {
                println!("{}", colorize_compose_line(&line, nc));
            }
        }

        let status = child.wait()?;
        let code = exit_code(&status);
        if !status.success() && code != 130 {
            anyhow::bail!("docker compose command failed (exit {})", status);
        }
        Ok(())
    }

    fn compose_capture(&self, ctx: &BackendCtx, args: &[&str]) -> Result<String> {
        // Read-only ps queries go straight to the Engine socket when possible —
        // saves the ~85ms docker CLI spawn on every status/health/stats call.
        // Any failure silently falls back to the CLI below.
        if let Some(result) = super::docker_api::try_ps(&ctx.project.project_name, args) {
            match result {
                Ok(out) => {
                    log_verbose(ctx.verbose, "  ps answered via docker socket");
                    return Ok(out);
                }
                Err(e) => log_verbose(
                    ctx.verbose,
                    &format!("  socket ps unavailable ({e}); using docker CLI"),
                ),
            }
        }

        let compose_file = ctx.project.compose_file.to_string_lossy().into_owned();
        let mut cmd_args = vec!["compose", "-f", compose_file.as_str()];
        cmd_args.extend_from_slice(args);

        log_cmd(ctx.verbose, &cmd_args);

        let output = Command::new("docker")
            .args(&cmd_args)
            .envs(ctx.project.get_env())
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("docker compose failed: {}", stderr.trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn raw_capture(&self, args: &[&str]) -> Result<String> {
        let output = Command::new("docker").args(args).output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("docker command failed: {}", stderr.trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn raw_stream(&self, args: &[&str]) -> Result<()> {
        let status = Command::new("docker").args(args).status()?;
        let code = exit_code(&status);
        if !status.success() && code != 130 {
            anyhow::bail!("docker command failed (exit {})", status);
        }
        Ok(())
    }

    fn db_services(&self, ctx: &BackendCtx) -> Result<Vec<DbService>> {
        docker_db_services(ctx)
    }

    fn project_containers(
        &self,
        ctx: &BackendCtx,
        include_stopped: bool,
    ) -> Result<Vec<RuntimeProjectContainer>> {
        docker_project_containers(ctx, include_stopped)
    }

    fn running_count_by_project(&self, project_name: &str) -> usize {
        docker_running_count_by_project(project_name)
    }

    fn container_ip(&self, container_id: &str) -> Result<String> {
        docker_container_ip(container_id)
    }

    fn prune_plan(&self, volumes: bool, all: bool) -> Vec<&'static str> {
        let mut what = vec![
            "stopped containers",
            "unused networks",
            if all {
                "all unused images"
            } else {
                "dangling images"
            },
            "build cache",
        ];
        if volumes {
            what.push("unused volumes (including named db_data etc.)");
        }
        what
    }

    fn prune_system(&self, volumes: bool, all: bool, out: &Output) -> Result<()> {
        run_spinner_command(
            "docker",
            &["container", "prune", "-f"],
            out,
            "Removing stopped containers...",
        );
        run_spinner_command(
            "docker",
            &["builder", "prune", "-f"],
            out,
            "Pruning build cache...",
        );
        run_spinner_command(
            "docker",
            &["network", "prune", "-f"],
            out,
            "Pruning unused networks...",
        );

        let mut image_args = vec!["image", "prune", "-f"];
        if all {
            image_args.push("-a");
        }
        run_spinner_command("docker", &image_args, out, "Pruning images...");

        if volumes {
            run_spinner_command(
                "docker",
                &["volume", "prune", "-f", "--all"],
                out,
                "Pruning unused volumes...",
            );
        }

        Ok(())
    }

    fn bench(&self, spec: &RuntimeBenchSpec) -> Result<RuntimeBenchMeasurement> {
        docker_bench(spec)
    }

    fn bench_steady(&self, spec: &RuntimeBenchSpec) -> Result<RuntimeBenchMeasurement> {
        docker_bench_steady(spec)
    }

    fn system_info(&self) -> Result<RuntimeSystemInfo> {
        docker_system_info()
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn log_cmd(verbose: bool, args: &[&str]) {
    log_verbose(verbose, &format!("  docker {}", args.join(" ")));
}

fn is_env_warning(line: &str) -> bool {
    line.contains("variable is not set. Defaulting to a blank string.")
}

fn docker_db_services(ctx: &BackendCtx) -> Result<Vec<DbService>> {
    let compose_file = ctx.project.compose_file.to_string_lossy().into_owned();

    let ps_out = Command::new("docker")
        .args(["compose", "-f", &compose_file, "ps", "-q"])
        .envs(ctx.project.get_env())
        .output()?;

    let ids: Vec<&str> = std::str::from_utf8(&ps_out.stdout)?
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    if ids.is_empty() {
        return Ok(vec![]);
    }

    log_verbose(ctx.verbose, &format!("  docker inspect {}", ids.join(" ")));

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
            Some(id) => id.get(..12).unwrap_or(id).to_string(),
            None => continue,
        };

        let service_name = labels["com.docker.compose.service"]
            .as_str()
            .unwrap_or_else(|| c["Name"].as_str().unwrap_or("db").trim_start_matches('/'))
            .to_string();

        let env = env::parse_env_json_array(&c["Config"]["Env"]);
        if let Some(service) = db_service_from_parts(service_name, container_id, db_type, &env) {
            services.push(service);
        }
    }

    Ok(services)
}

fn db_service_from_parts(
    service_name: String,
    container_id: String,
    db_type: &str,
    env: &std::collections::HashMap<String, String>,
) -> Option<DbService> {
    match db_type {
        "mysql" => {
            let db_name = env.get("MYSQL_DATABASE").cloned().unwrap_or_default();
            let password = env.get("MYSQL_ROOT_PASSWORD").cloned().unwrap_or_default();
            if db_name.is_empty() || password.is_empty() {
                return None;
            }
            Some(DbService {
                service_name,
                container_id,
                backend: Box::new(MySqlBackend),
                config: DbConfig {
                    db_name,
                    password,
                    user: "root".to_string(),
                },
            })
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
                return None;
            }
            let user = env
                .get("POSTGRES_USER")
                .or_else(|| env.get("PGUSER"))
                .cloned()
                .unwrap_or_else(|| "postgres".to_string());
            Some(DbService {
                service_name,
                container_id,
                backend: Box::new(PostgresBackend),
                config: DbConfig {
                    db_name,
                    password,
                    user,
                },
            })
        }
        _ => None,
    }
}

fn docker_container_ip(container_id: &str) -> Result<String> {
    let out = Command::new("docker")
        .args(["inspect", container_id])
        .output()?;
    if !out.status.success() {
        anyhow::bail!("docker inspect failed for container {container_id}");
    }
    let json: Value = serde_json::from_slice(&out.stdout)?;
    json[0]["NetworkSettings"]["Networks"]
        .as_object()
        .and_then(|nets| nets.values().next())
        .and_then(|net| net["IPAddress"].as_str())
        .filter(|ip| !ip.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("Could not find IP for container {container_id}"))
}

fn docker_project_containers(
    ctx: &BackendCtx,
    include_stopped: bool,
) -> Result<Vec<RuntimeProjectContainer>> {
    let compose = ctx.project.compose_file.to_string_lossy().into_owned();
    let mut ps_args = vec!["compose", "-f", compose.as_str(), "ps", "-q"];
    if include_stopped {
        ps_args.push("-a");
    }
    let ps = Command::new("docker")
        .args(&ps_args)
        .envs(ctx.project.get_env())
        .output()?;

    let ids: Vec<String> = String::from_utf8_lossy(&ps.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();

    if ids.is_empty() {
        return Ok(vec![]);
    }

    let mut args = vec!["inspect".to_string()];
    args.extend(ids);
    let inspect = Command::new("docker").args(&args).output()?;
    if !inspect.status.success() {
        return Ok(vec![]);
    }

    let json: Value = serde_json::from_slice(&inspect.stdout)?;
    let mut containers = Vec::new();
    for container in json.as_array().into_iter().flatten() {
        let ip = container["NetworkSettings"]["Networks"]
            .as_object()
            .and_then(|nets| nets.values().next())
            .and_then(|net| net["IPAddress"].as_str())
            .filter(|ip| !ip.is_empty())
            .map(str::to_string);

        containers.push(RuntimeProjectContainer {
            labels: container["Config"]["Labels"].clone(),
            ip,
        });
    }

    Ok(containers)
}

fn docker_running_count_by_project(project_name: &str) -> usize {
    let Ok(output) = Command::new("docker")
        .args([
            "ps",
            "-q",
            "--filter",
            &format!("label=com.docker.compose.project={project_name}"),
        ])
        .output()
    else {
        return 0;
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .count()
}

fn docker_bench(spec: &RuntimeBenchSpec) -> Result<RuntimeBenchMeasurement> {
    ensure_docker_image(&spec.image)?;
    let script = bench_disk_script(&spec.path, spec.size_mb);
    let mut totals = RuntimeBenchTotals::default();
    let runs = spec.warmup + spec.iterations.max(1);

    for index in 0..runs {
        let measured = index >= spec.warmup;
        let name = bench_container_name("docker");
        let _ = docker_status(&["rm", "-f", &name]);

        let result = (|| -> Result<()> {
            let started = Instant::now();
            let args = docker_run_args(&name, spec);
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            docker_status(&arg_refs)?;
            let start_elapsed = started.elapsed();

            let exec_started = Instant::now();
            docker_status(&["exec", &name, "true"])?;
            let exec_elapsed = exec_started.elapsed();

            let disk_started = Instant::now();
            docker_status(&["exec", &name, "sh", "-lc", &script])?;
            let disk_elapsed = disk_started.elapsed();

            if measured {
                totals.start += start_elapsed;
                totals.exec += exec_elapsed;
                totals.disk += disk_elapsed;
            }
            Ok(())
        })();

        let _ = docker_status(&["rm", "-f", &name]);
        result?;
    }

    Ok(bench_measurement("docker", spec, totals))
}

fn docker_bench_steady(spec: &RuntimeBenchSpec) -> Result<RuntimeBenchMeasurement> {
    ensure_docker_image(&spec.image)?;
    let script = bench_disk_script(&spec.path, spec.size_mb);
    let mut totals = RuntimeBenchTotals::default();
    let runs = spec.warmup + spec.iterations.max(1);
    let name = bench_container_name("docker");
    let _ = docker_status(&["rm", "-f", &name]);

    let result = (|| -> Result<()> {
        let started = Instant::now();
        let args = docker_run_args(&name, spec);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        docker_status(&arg_refs)?;
        totals.start = started.elapsed();

        for index in 0..runs {
            let measured = index >= spec.warmup;

            let exec_started = Instant::now();
            docker_status(&["exec", &name, "true"])?;
            let exec_elapsed = exec_started.elapsed();

            let disk_started = Instant::now();
            docker_status(&["exec", &name, "sh", "-lc", &script])?;
            let disk_elapsed = disk_started.elapsed();

            if measured {
                totals.exec += exec_elapsed;
                totals.disk += disk_elapsed;
            }
        }

        Ok(())
    })();

    let _ = docker_status(&["rm", "-f", &name]);
    result?;

    Ok(steady_bench_measurement("docker", spec, totals))
}

fn docker_run_args(name: &str, spec: &RuntimeBenchSpec) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--detach".to_string(),
        "--name".to_string(),
        name.to_string(),
    ];
    if let Some(mount) = &spec.mount {
        args.push("--volume".to_string());
        args.push(format!(
            "{}:{}",
            mount.host_path.to_string_lossy(),
            mount.container_path
        ));
    }
    args.extend([
        spec.image.clone(),
        "sh".to_string(),
        "-c".to_string(),
        "sleep 3600".to_string(),
    ]);
    args
}

fn ensure_docker_image(image: &str) -> Result<()> {
    if docker_status(&["image", "inspect", image]).is_ok() {
        return Ok(());
    }
    docker_status(&["pull", image])
}

fn docker_status(args: &[&str]) -> Result<()> {
    let output = Command::new("docker").args(args).output()?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::bail!(
        "docker {} failed: {}",
        args.join(" "),
        if message.is_empty() {
            output.status.to_string()
        } else {
            message
        }
    )
}

fn run_spinner_command(binary: &str, args: &[&str], out: &Output, msg: &str) {
    let spinner = out.spinner(msg);
    let _ = Command::new(binary)
        .args(args)
        .stdout(Stdio::null())
        .status();
    spinner.finish_and_clear();
}

fn bench_container_name(runtime: &str) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    format!("dip-bench-{runtime}-{}-{millis}", std::process::id())
}

fn docker_system_info() -> Result<RuntimeSystemInfo> {
    let version_raw = Command::new("docker")
        .args([
            "version",
            "--format",
            "Client: {{.Client.Version}}, Server: {{.Server.Version}}",
        ])
        .output()?;
    let version = String::from_utf8_lossy(&version_raw.stdout)
        .trim()
        .to_string();

    let info_raw = Command::new("docker")
        .args([
            "info",
            "--format",
            "{{.Containers}}\n{{.ContainersRunning}}\n{{.ContainersPaused}}\n{{.ContainersStopped}}\n{{.Images}}",
        ])
        .output()?;
    let info = String::from_utf8_lossy(&info_raw.stdout).into_owned();
    let lines: Vec<&str> = info.lines().collect();

    Ok(RuntimeSystemInfo {
        version_label: "Docker:",
        version,
        images: lines.get(4).map(|value| (*value).to_string()),
        containers: if lines.len() >= 4 {
            Some(RuntimeContainerCounts {
                total: lines[0].to_string(),
                running: lines[1].to_string(),
                paused: lines[2].to_string(),
                stopped: lines[3].to_string(),
            })
        } else {
            None
        },
    })
}
