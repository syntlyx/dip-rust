use std::collections::{BTreeMap, BTreeSet};
use std::io::BufRead;
use std::process::{Command, Stdio};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use colored::Colorize;
use serde_json::Value;

use crate::db::{DbConfig, DbService, MySqlBackend, PostgresBackend};
use crate::runtime::compose_file::{self, ComposeConfig, ServiceConfig};
use crate::utils::env;
use crate::utils::log_verbose;
use crate::utils::output::{Output, colorize_compose_line, service_color};

use super::{
    BackendCtx, ContainerRuntime, RuntimeBenchMeasurement, RuntimeBenchSpec, RuntimeBenchTotals,
    RuntimeProjectContainer, RuntimeSystemInfo, bench_disk_script, bench_measurement,
    steady_bench_measurement,
};

pub struct AppleContainerRuntime;

impl ContainerRuntime for AppleContainerRuntime {
    fn name(&self) -> &'static str {
        "apple"
    }

    fn check_daemon(&self) -> Result<()> {
        let ok = Command::new("container")
            .args(["system", "status"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            Ok(())
        } else {
            anyhow::bail!(
                "Apple Container is not running. Install it and run `container system start`."
            )
        }
    }

    fn compose_run(&self, ctx: &BackendCtx, args: &[&str], msg: &str) -> Result<()> {
        log_verbose(
            ctx.verbose,
            &format!("  apple runtime compose shim: {}", args.join(" ")),
        );
        println!("{msg}");

        match args {
            ["build"] => build_services(ctx, None),
            ["build", service] => build_services(ctx, Some(service)),
            ["up", "-d"] => up_services(ctx, None),
            ["up", "-d", service] => up_services(ctx, Some(service)),
            ["stop"] => stop_services(ctx, None),
            ["stop", service] => stop_services(ctx, Some(service)),
            ["rm", ..] => remove_services(ctx, service_arg_after_rm(args)),
            ["pull"] => pull_services(ctx),
            other => unsupported_compose(other),
        }
    }

    fn compose_stream(&self, ctx: &BackendCtx, args: &[&str], passthrough: bool) -> Result<i32> {
        match args.first().copied() {
            Some("logs") => stream_logs(ctx, args, &[]).map(|_| 0),
            Some("exec") => exec_service(ctx, args, passthrough),
            Some("top") => top_services(ctx, args).map(|_| 0),
            _ => unsupported_compose::<i32>(args),
        }
    }

    fn compose_stream_grep(
        &self,
        ctx: &BackendCtx,
        args: &[&str],
        keywords: &[&str],
    ) -> Result<()> {
        match args.first().copied() {
            Some("logs") => stream_logs(ctx, args, keywords),
            _ => unsupported_compose(args),
        }
    }

    fn compose_capture(&self, ctx: &BackendCtx, args: &[&str]) -> Result<String> {
        match args {
            ["config", "--format", "json"] => {
                let config = compose_file::load_project_compose(ctx.project)?;
                serde_json::to_string(&config).context("failed to serialize Compose config")
            }
            ["config", "--services"] => {
                let config = compose_file::load_project_compose(ctx.project)?;
                Ok(config
                    .services
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n")
            }
            ["ps", "-q", "-a"] => ps_quiet(ctx, None, true),
            ["ps", "-q", "-a", service] => ps_quiet(ctx, Some(service), true),
            ["ps", "-q"] => ps_quiet(ctx, None, false),
            ["ps", "-q", service] => ps_quiet(ctx, Some(service), false),
            ["ps", "-a", "--format", "json"] => ps_json(ctx),
            other => unsupported_compose(other),
        }
    }

    fn raw_capture(&self, args: &[&str]) -> Result<String> {
        if let Some(project) = parse_docker_project_filter(args) {
            return apple_ps_quiet_by_project(project);
        }

        let output = Command::new("container").args(args).output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("container command failed: {}", stderr.trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn raw_stream(&self, args: &[&str]) -> Result<()> {
        let mut translated = Vec::new();
        if args.first() == Some(&"stats") {
            translated.push("stats");
            for arg in args.iter().skip(1) {
                if *arg == "--format" {
                    continue;
                }
                if arg.starts_with("table ") {
                    continue;
                }
                translated.push(arg);
            }
        } else if args.first() == Some(&"image") && args.get(1) == Some(&"prune") {
            translated.push("image");
            translated.push("prune");
            for arg in args.iter().skip(2) {
                match *arg {
                    "-f" | "--force" => {}
                    "-a" => translated.push("--all"),
                    other => translated.push(other),
                }
            }
        } else {
            translated.extend_from_slice(args);
        }

        run_container_status(&translated, false)
    }

    fn db_services(&self, ctx: &BackendCtx) -> Result<Vec<DbService>> {
        apple_db_services(ctx)
    }

    fn project_containers(
        &self,
        ctx: &BackendCtx,
        include_stopped: bool,
    ) -> Result<Vec<RuntimeProjectContainer>> {
        apple_project_containers(ctx, include_stopped)
    }

    fn running_count_by_project(&self, project_name: &str) -> usize {
        apple_running_count_by_project(project_name)
    }

    fn container_ip(&self, container_id: &str) -> Result<String> {
        apple_container_ip(container_id)
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
        ];
        if volumes {
            what.push("unused volumes (including named db_data etc.)");
        }
        what
    }

    fn prune_system(&self, volumes: bool, all: bool, out: &Output) -> Result<()> {
        run_spinner_command(
            "container",
            &["prune"],
            out,
            "Removing stopped containers...",
        );
        run_spinner_command(
            "container",
            &["network", "prune"],
            out,
            "Pruning unused networks...",
        );

        let mut image_args = vec!["image", "prune"];
        if all {
            image_args.push("--all");
        }
        run_spinner_command("container", &image_args, out, "Pruning images...");

        if volumes {
            run_spinner_command(
                "container",
                &["volume", "prune"],
                out,
                "Pruning unused volumes...",
            );
        }
        Ok(())
    }

    fn bench(&self, spec: &RuntimeBenchSpec) -> Result<RuntimeBenchMeasurement> {
        apple_bench(spec)
    }

    fn bench_steady(&self, spec: &RuntimeBenchSpec) -> Result<RuntimeBenchMeasurement> {
        apple_bench_steady(spec)
    }

    fn system_info(&self) -> Result<RuntimeSystemInfo> {
        apple_system_info()
    }
}

fn build_services(ctx: &BackendCtx, service: Option<&str>) -> Result<()> {
    let config = compose_file::load_project_compose(ctx.project)?;
    for name in selected_services(&config, service)? {
        let Some(service) = config.services.get(&name) else {
            continue;
        };
        if service.build.is_some() {
            build_service(ctx, &name, service)?;
        }
    }
    Ok(())
}

fn up_services(ctx: &BackendCtx, service: Option<&str>) -> Result<()> {
    let config = compose_file::load_project_compose(ctx.project)?;
    let network = ensure_network(ctx)?;

    for name in start_order(&config, service)? {
        let Some(service) = config.services.get(&name) else {
            continue;
        };
        if service.build.is_some() && !image_exists(&image_name(ctx, &name, service)) {
            build_service(ctx, &name, service)?;
        }
        run_service(ctx, &name, service, &network)?;
    }

    Ok(())
}

fn stop_services(ctx: &BackendCtx, service: Option<&str>) -> Result<()> {
    let config = compose_file::load_project_compose(ctx.project)?;
    for name in selected_services(&config, service)? {
        let container = container_name(ctx, &name);
        if container_exists(&container) {
            run_container_status(&["stop", &container], ctx.verbose).ok();
        }
    }
    Ok(())
}

fn remove_services(ctx: &BackendCtx, service: Option<&str>) -> Result<()> {
    let config = compose_file::load_project_compose(ctx.project)?;
    for name in selected_services(&config, service)? {
        let container = container_name(ctx, &name);
        delete_container_if_exists(ctx, &container);
    }
    Ok(())
}

fn pull_services(ctx: &BackendCtx) -> Result<()> {
    let config = compose_file::load_project_compose(ctx.project)?;
    for service in config.services.values() {
        if service.build.is_none()
            && let Some(image) = service.image.as_deref()
        {
            run_container_status(&["image", "pull", image], ctx.verbose)?;
        }
    }
    Ok(())
}

fn build_service(ctx: &BackendCtx, service_name: &str, service: &ServiceConfig) -> Result<()> {
    let Some(build) = &service.build else {
        return Ok(());
    };
    let image = image_name(ctx, service_name, service);
    let mut args = vec!["build".to_string(), "-t".to_string(), image];

    if let Some(path) = build.dockerfile_path() {
        args.push("-f".to_string());
        args.push(path.to_string_lossy().into_owned());
    }
    if let Some(target) = build.target.as_deref() {
        args.push("--target".to_string());
        args.push(target.to_string());
    }
    for (key, value) in &build.args {
        if let Some(value) = value_to_string(value) {
            args.push("--build-arg".to_string());
            args.push(format!("{key}={value}"));
        }
    }

    let context = build
        .context_path()
        .ok_or_else(|| anyhow::anyhow!("{service_name}: build context is missing"))?;
    args.push(context.to_string_lossy().into_owned());

    run_container_owned(ctx, &args)
}

fn run_service(
    ctx: &BackendCtx,
    service_name: &str,
    service: &ServiceConfig,
    network: &str,
) -> Result<()> {
    let container = container_name(ctx, service_name);
    delete_container_if_exists(ctx, &container);

    let image = image_name(ctx, service_name, service);
    let mut args = vec![
        "run".to_string(),
        "--detach".to_string(),
        "--name".to_string(),
        container,
        "--network".to_string(),
        network.to_string(),
        "--label".to_string(),
        format!("com.docker.compose.project={}", ctx.project.project_name),
        "--label".to_string(),
        format!("com.docker.compose.service={service_name}"),
        "--label".to_string(),
        format!("com.dip.project={}", ctx.project.project_name),
        "--label".to_string(),
        format!("com.dip.service={service_name}"),
    ];

    for (key, value) in service.label_entries() {
        args.push("--label".to_string());
        args.push(format!("{key}={value}"));
    }
    for env_file in string_list(&service.env_file) {
        args.push("--env-file".to_string());
        args.push(env_file);
    }
    for (key, value) in environment_entries(&service.environment) {
        args.push("--env".to_string());
        args.push(format!("{key}={value}"));
    }
    for volume in &service.volumes {
        if let Some(spec) = volume_spec(volume) {
            args.push("--volume".to_string());
            args.push(spec);
        }
    }
    for port in string_list(&service.ports) {
        args.push("--publish".to_string());
        args.push(port);
    }
    if let Some(workdir) = service.working_dir.as_deref() {
        args.push("--workdir".to_string());
        args.push(workdir.to_string());
    }
    if let Some(entrypoint) = entrypoint_arg(&service.entrypoint) {
        args.push("--entrypoint".to_string());
        args.push(entrypoint);
    }

    args.push(image);
    args.extend(command_args(&service.command));

    match run_container_owned_capture(ctx, &args) {
        Ok(()) => Ok(()),
        Err(message) if network != "default" && is_network_not_found(&message) => {
            log_verbose(
                ctx.verbose,
                &format!(
                    "  [warn] Apple Container network {network} was accepted by inspect/list but rejected by run; retrying on default"
                ),
            );
            replace_run_network(&mut args, "default");
            run_container_owned_capture(ctx, &args).map_err(|e| anyhow::anyhow!("{e}"))
        }
        Err(message) => Err(anyhow::anyhow!("{message}")),
    }
}

fn ensure_network(ctx: &BackendCtx) -> Result<String> {
    let network = network_name(ctx);
    if network_ready(&network) {
        return Ok(network);
    }

    let create = Command::new("container")
        .args(["network", "create", &network])
        .stdout(if ctx.verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .stderr(if ctx.verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .status();

    match create {
        Ok(status) if status.success() => {
            for _ in 0..30 {
                if network_ready(&network) {
                    return Ok(network);
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            log_verbose(
                ctx.verbose,
                &format!("  [warn] Apple Container network {network} was not ready; using default"),
            );
            Ok("default".to_string())
        }
        Ok(status) => {
            log_verbose(
                ctx.verbose,
                &format!("  [warn] container network create failed ({status}); using default"),
            );
            Ok("default".to_string())
        }
        Err(e) => {
            log_verbose(
                ctx.verbose,
                &format!("  [warn] container network create failed: {e}; using default"),
            );
            Ok("default".to_string())
        }
    }
}

fn exec_service(ctx: &BackendCtx, args: &[&str], passthrough: bool) -> Result<i32> {
    let Some(parsed) = parse_exec_service(args) else {
        anyhow::bail!(
            "apple runtime: unsupported exec command: {}",
            args.join(" ")
        );
    };
    let container = container_name(ctx, parsed.service);
    let mut translated = vec!["exec".to_string()];
    translated.extend(parsed.options);
    translated.push(container);
    translated.extend(
        args[parsed.command_start..]
            .iter()
            .map(|s| (*s).to_string()),
    );

    let status = command_owned(&translated)
        .status()
        .context("failed to run container exec")?;
    let code = status.code().unwrap_or(1);
    if !status.success() && code != 130 && !passthrough {
        anyhow::bail!("container exec failed (exit {})", status);
    }
    Ok(code)
}

fn stream_logs(ctx: &BackendCtx, args: &[&str], keywords: &[&str]) -> Result<()> {
    let config = compose_file::load_project_compose(ctx.project)?;
    let targets = log_targets(&config, args)?;
    let follow = args.iter().any(|arg| *arg == "-f" || *arg == "--follow");
    if follow && targets.len() > 1 {
        return stream_multi_logs(ctx, &targets, args, keywords);
    }
    for target in targets {
        let container = container_name(ctx, &target);
        let mut translated = vec!["logs".to_string()];
        if follow {
            translated.push("--follow".to_string());
        }
        if let Some(tail) = args.iter().find_map(|arg| arg.strip_prefix("--tail=")) {
            translated.push("-n".to_string());
            translated.push(tail.to_string());
        }
        translated.push(container);

        stream_container_output(ctx, &translated, keywords)?;
    }
    Ok(())
}

fn stream_multi_logs(
    ctx: &BackendCtx,
    targets: &[String],
    args: &[&str],
    keywords: &[&str],
) -> Result<()> {
    let mut children = Vec::new();
    let mut handles = Vec::new();

    for target in targets {
        let container = container_name(ctx, target);
        let mut translated = vec!["logs".to_string(), "--follow".to_string()];
        if let Some(tail) = args.iter().find_map(|arg| arg.strip_prefix("--tail=")) {
            translated.push("-n".to_string());
            translated.push(tail.to_string());
        }
        translated.push(container);

        let refs: Vec<&str> = translated.iter().map(String::as_str).collect();
        log_container_cmd(ctx.verbose, &refs);
        let mut child = command_owned(&translated)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to stream container logs")?;

        if let Some(stdout) = child.stdout.take() {
            handles.push(spawn_log_reader(
                target.clone(),
                stdout,
                keywords,
                ctx.no_color,
            ));
        }
        if let Some(stderr) = child.stderr.take() {
            handles.push(spawn_log_reader(
                target.clone(),
                stderr,
                keywords,
                ctx.no_color,
            ));
        }

        children.push(child);
    }

    let mut failed = None;
    for mut child in children {
        let status = child.wait()?;
        let code = status.code().unwrap_or(0);
        if !status.success() && code != 130 {
            failed = Some(status);
        }
    }

    for handle in handles {
        if handle.join().is_err() {
            log_verbose(ctx.verbose, "  [warn] log reader thread panicked");
        }
    }

    if let Some(status) = failed {
        anyhow::bail!("container logs failed (exit {})", status);
    }

    Ok(())
}

fn spawn_log_reader<R>(
    service: String,
    stream: R,
    keywords: &[&str],
    no_color: bool,
) -> std::thread::JoinHandle<()>
where
    R: std::io::Read + Send + 'static,
{
    let keywords: Vec<String> = keywords.iter().map(|kw| kw.to_ascii_lowercase()).collect();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stream)
            .lines()
            .map_while(Result::ok)
        {
            let lower = line.to_ascii_lowercase();
            if !keywords.is_empty() && !keywords.iter().any(|kw| lower.contains(kw)) {
                continue;
            }

            let prefix = if no_color {
                format!("{:<12}", service)
            } else {
                format!("{:<12}", service)
                    .color(service_color(&service))
                    .bold()
                    .to_string()
            };
            println!("  {} {}", prefix, colorize_compose_line(&line, no_color));
        }
    })
}

fn top_services(ctx: &BackendCtx, args: &[&str]) -> Result<()> {
    let config = compose_file::load_project_compose(ctx.project)?;
    let targets = if let Some(service) = args.get(1) {
        vec![(*service).to_string()]
    } else {
        config.services.keys().cloned().collect()
    };

    for service in targets {
        let container = container_name(ctx, &service);
        println!("{service}:");
        run_container_owned(
            ctx,
            &[
                "exec".to_string(),
                container,
                "ps".to_string(),
                "aux".to_string(),
            ],
        )?;
    }
    Ok(())
}

fn ps_quiet(ctx: &BackendCtx, service: Option<&str>, all: bool) -> Result<String> {
    let config = compose_file::load_project_compose(ctx.project)?;
    let mut out = String::new();
    for name in selected_services(&config, service)? {
        let container = container_name(ctx, &name);
        if all || container_running(&container) {
            out.push_str(&container);
            out.push('\n');
        }
    }
    Ok(out)
}

fn ps_json(ctx: &BackendCtx) -> Result<String> {
    let config = compose_file::load_project_compose(ctx.project)?;
    let containers = list_containers_json().unwrap_or_default();
    let mut by_id = BTreeMap::new();
    for container in containers {
        if let Some(id) = apple_container_id(&container) {
            by_id.insert(id, container);
        }
    }

    let mut lines = Vec::new();
    for (service, service_config) in &config.services {
        let id = container_name(ctx, service);
        let state = by_id
            .get(&id)
            .and_then(|v| json_string_any(v, &["STATE", "State", "state", "status"]))
            .unwrap_or_else(|| "exited".to_string());
        let normalized_state = normalize_state(&state);
        let status = by_id
            .get(&id)
            .and_then(|v| json_string_any(v, &["STATUS", "Status", "status"]))
            .unwrap_or_default();
        let health = if normalized_state == "running" {
            run_healthcheck(ctx, &id, service_config)
        } else {
            String::new()
        };
        let row = serde_json::json!({
            "Service": service,
            "State": normalized_state,
            "Status": status,
            "Health": health,
            "Ports": string_list(&service_config.ports).join(", "),
        });
        lines.push(serde_json::to_string(&row)?);
    }

    Ok(lines.join("\n") + "\n")
}

fn apple_db_services(ctx: &BackendCtx) -> Result<Vec<DbService>> {
    log_verbose(ctx.verbose, "  container list --all --format json");
    let containers = list_containers_json()?;
    let mut services = vec![];

    for c in containers {
        if normalize_state(
            &json_string_any(&c, &["STATE", "State", "state", "status"]).unwrap_or_default(),
        ) != "running"
        {
            continue;
        }

        let labels = c.pointer("/configuration/labels").unwrap_or(&Value::Null);
        let belongs_to_project = labels
            .get("com.docker.compose.project")
            .or_else(|| labels.get("com.dip.project"))
            .and_then(Value::as_str)
            == Some(ctx.project.project_name.as_str());
        if !belongs_to_project {
            continue;
        }

        let db_type = match labels["dip.db"].as_str() {
            Some(t) => t,
            None => continue,
        };

        let container_id = match c.pointer("/configuration/id").and_then(Value::as_str) {
            Some(id) => id.to_string(),
            None => continue,
        };

        let service_name = labels["com.docker.compose.service"]
            .as_str()
            .or_else(|| labels["com.dip.service"].as_str())
            .unwrap_or("db")
            .to_string();

        let env = env::parse_env_json_array(
            c.pointer("/configuration/initProcess/environment")
                .unwrap_or(&Value::Null),
        );
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

fn run_healthcheck(ctx: &BackendCtx, container: &str, service: &ServiceConfig) -> String {
    let Some(args) = healthcheck_command_args(service) else {
        return String::new();
    };

    log_verbose(
        ctx.verbose,
        &format!("  container exec {} {}", container, args.join(" ")),
    );
    let ok = Command::new("container")
        .arg("exec")
        .arg(container)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);

    if ok {
        "healthy".to_string()
    } else {
        "unhealthy".to_string()
    }
}

fn healthcheck_command_args(service: &ServiceConfig) -> Option<Vec<String>> {
    let test = service.healthcheck.get("test").unwrap_or(&Value::Null);
    match test {
        Value::String(command) if !command.trim().is_empty() => Some(vec![
            "sh".to_string(),
            "-c".to_string(),
            command.trim().to_string(),
        ]),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(value_to_string).collect();
            match parts.first().map(String::as_str) {
                Some("NONE") | None => None,
                Some("CMD") => Some(parts.into_iter().skip(1).collect()),
                Some("CMD-SHELL") => Some(vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    parts.into_iter().skip(1).collect::<Vec<_>>().join(" "),
                ]),
                _ => Some(parts),
            }
        }
        _ => None,
    }
}

fn selected_services(config: &ComposeConfig, service: Option<&str>) -> Result<Vec<String>> {
    if let Some(service) = service {
        if !config.services.contains_key(service) {
            anyhow::bail!("Service '{service}' not found");
        }
        return Ok(vec![service.to_string()]);
    }

    Ok(config.services.keys().cloned().collect())
}

fn start_order(config: &ComposeConfig, service: Option<&str>) -> Result<Vec<String>> {
    let roots = selected_services(config, service)?;
    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    for service in roots {
        visit_service(config, &service, &mut seen, &mut order)?;
    }
    Ok(order)
}

fn visit_service(
    config: &ComposeConfig,
    service: &str,
    seen: &mut BTreeSet<String>,
    order: &mut Vec<String>,
) -> Result<()> {
    if !seen.insert(service.to_string()) {
        return Ok(());
    }
    let svc = config
        .services
        .get(service)
        .ok_or_else(|| anyhow::anyhow!("Service '{service}' not found"))?;
    for dependency in &svc.depends_on {
        if config.services.contains_key(dependency) {
            visit_service(config, dependency, seen, order)?;
        }
    }
    order.push(service.to_string());
    Ok(())
}

struct ExecParse<'a> {
    service: &'a str,
    command_start: usize,
    options: Vec<String>,
}

fn parse_exec_service<'a>(args: &'a [&'a str]) -> Option<ExecParse<'a>> {
    let mut index = 1;
    let mut options = Vec::new();
    while index < args.len() {
        let arg = args[index];
        match arg {
            "-i" | "--interactive" => {
                options.push("--interactive".to_string());
                index += 1;
            }
            "-t" | "--tty" => {
                options.push("--tty".to_string());
                index += 1;
            }
            "-it" | "-ti" => {
                options.push("--interactive".to_string());
                options.push("--tty".to_string());
                index += 1;
            }
            "-e" | "--env" | "--env-file" | "-u" | "--user" | "--uid" | "--gid" | "--ulimit"
            | "-w" | "--workdir" | "--cwd" => {
                let value = args.get(index + 1)?;
                options.push(exec_option_name(arg).to_string());
                options.push((*value).to_string());
                index += 2;
            }
            _ if arg.starts_with("--env=") => {
                options.push("--env".to_string());
                options.push(arg.trim_start_matches("--env=").to_string());
                index += 1;
            }
            _ if arg.starts_with("--env-file=") => {
                options.push("--env-file".to_string());
                options.push(arg.trim_start_matches("--env-file=").to_string());
                index += 1;
            }
            _ if arg.starts_with("--workdir=") || arg.starts_with("--cwd=") => {
                let (_, value) = arg.split_once('=')?;
                options.push("--workdir".to_string());
                options.push(value.to_string());
                index += 1;
            }
            _ if arg.starts_with('-') => {
                index += 1;
            }
            _ => {
                return Some(ExecParse {
                    service: arg,
                    command_start: index + 1,
                    options,
                });
            }
        }
    }
    None
}

fn exec_option_name(arg: &str) -> &str {
    match arg {
        "-e" => "--env",
        "-u" => "--user",
        "-w" | "--cwd" => "--workdir",
        other => other,
    }
}

fn log_targets(config: &ComposeConfig, args: &[&str]) -> Result<Vec<String>> {
    let maybe_service = args.iter().rev().find(|arg| {
        !arg.starts_with('-') && **arg != "logs" && config.services.contains_key(**arg)
    });
    selected_services(config, maybe_service.copied())
}

fn service_arg_after_rm<'a>(args: &'a [&'a str]) -> Option<&'a str> {
    args.iter()
        .skip(1)
        .rev()
        .find(|arg| !arg.starts_with('-'))
        .copied()
}

fn image_name(ctx: &BackendCtx, service_name: &str, service: &ServiceConfig) -> String {
    service
        .image
        .clone()
        .unwrap_or_else(|| format!("{}:latest", container_name(ctx, service_name)))
}

fn container_name(ctx: &BackendCtx, service_name: &str) -> String {
    format!(
        "dip-{}-{}",
        sanitize_name(&ctx.project.project_name),
        sanitize_name(service_name)
    )
}

fn network_name(ctx: &BackendCtx) -> String {
    format!("dip-{}", sanitize_name(&ctx.project.project_name))
}

fn sanitize_name(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        let next = if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
            ch.to_ascii_lowercase()
        } else {
            '-'
        };
        if next == '-' {
            if !last_dash {
                out.push(next);
            }
            last_dash = true;
        } else {
            out.push(next);
            last_dash = false;
        }
    }
    out.trim_matches('-').to_string()
}

fn environment_entries(value: &Value) -> Vec<(String, String)> {
    let Some(map) = value.as_object() else {
        return vec![];
    };
    map.iter()
        .filter_map(|(key, value)| value_to_string(value).map(|v| (key.clone(), v)))
        .collect()
}

fn string_list(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items.iter().filter_map(value_to_string).collect::<Vec<_>>(),
        _ => vec![],
    }
}

fn command_args(value: &Value) -> Vec<String> {
    match value {
        Value::String(command) if !command.trim().is_empty() => {
            vec!["/bin/sh".to_string(), "-lc".to_string(), command.clone()]
        }
        Value::Array(items) => items.iter().filter_map(value_to_string).collect(),
        _ => vec![],
    }
}

fn entrypoint_arg(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(value_to_string).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(" "))
            }
        }
        _ => None,
    }
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn volume_spec(volume: &compose_file::VolumeConfig) -> Option<String> {
    let target = volume.target.as_deref()?;
    let readonly = if volume.read_only == Some(true) {
        ":ro"
    } else {
        ""
    };
    match volume.source.as_deref() {
        Some(source) => Some(format!("{source}:{target}{readonly}")),
        None => Some(target.to_string()),
    }
}

fn delete_container_if_exists(ctx: &BackendCtx, name: &str) {
    if !container_exists(name) {
        return;
    }

    if let Err(e) = run_container_capture(&["delete", "--force", name], ctx.verbose) {
        if is_container_not_found(&e) {
            log_verbose(
                ctx.verbose,
                &format!(
                    "  [warn] Apple Container inspect returned a stale entry for {name}; ignoring missing delete"
                ),
            );
            return;
        }

        log_verbose(
            ctx.verbose,
            &format!(
                "  [warn] failed to delete existing container {name}: {}",
                e.trim()
            ),
        );
    }
}

fn container_exists(name: &str) -> bool {
    let Ok(output) = Command::new("container").args(["inspect", name]).output() else {
        return false;
    };

    output.status.success() && json_output_has_entries(&output.stdout)
}

fn image_exists(name: &str) -> bool {
    let Ok(output) = Command::new("container")
        .args(["image", "inspect", name])
        .output()
    else {
        return false;
    };

    output.status.success() && json_output_has_entries(&output.stdout)
}

fn container_running(name: &str) -> bool {
    let Ok(containers) = list_containers_json() else {
        return false;
    };
    containers.into_iter().any(|container| {
        apple_container_id(&container).as_deref() == Some(name)
            && json_string_any(&container, &["STATE", "State", "state", "status"])
                .map(|state| normalize_state(&state) == "running")
                .unwrap_or(false)
    })
}

fn network_ready(name: &str) -> bool {
    if let Ok(output) = Command::new("container")
        .args(["network", "inspect", name])
        .output()
        && output.status.success()
        && json_output_has_entries(&output.stdout)
    {
        return true;
    }

    let Ok(output) = Command::new("container").args(["network", "list"]).output() else {
        return false;
    };

    if !output.status.success() {
        return false;
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .filter_map(|line| line.split_whitespace().next())
        .any(|network| network == name)
}

fn json_output_has_entries(output: &[u8]) -> bool {
    match serde_json::from_slice::<Value>(output) {
        Ok(Value::Array(items)) => !items.is_empty(),
        Ok(Value::Object(map)) => !map.is_empty(),
        _ => false,
    }
}

fn is_container_not_found(message: &str) -> bool {
    message.contains("notFound") || message.contains("not found")
}

fn is_network_not_found(message: &str) -> bool {
    message.contains("network") && message.contains("not found")
}

fn replace_run_network(args: &mut [String], network: &str) {
    for index in 0..args.len() {
        if args[index] == "--network"
            && let Some(value) = args.get_mut(index + 1)
        {
            *value = network.to_string();
            return;
        }
    }
}

fn list_containers_json() -> Result<Vec<Value>> {
    let output = Command::new("container")
        .args(["list", "--all", "--format", "json"])
        .output()
        .context("failed to list Apple containers")?;
    if !output.status.success() {
        return Ok(vec![]);
    }
    let value: Value = serde_json::from_slice(&output.stdout)?;
    match value {
        Value::Array(items) => Ok(items),
        Value::Object(mut map) => match map.remove("containers") {
            Some(Value::Array(items)) => Ok(items),
            _ => Ok(vec![]),
        },
        _ => Ok(vec![]),
    }
}

fn parse_docker_project_filter<'a>(args: &'a [&'a str]) -> Option<&'a str> {
    if args.len() != 4 || args[0] != "ps" || args[1] != "-q" || args[2] != "--filter" {
        return None;
    }
    args[3].strip_prefix("label=com.docker.compose.project=")
}

fn apple_ps_quiet_by_project(project: &str) -> Result<String> {
    let containers = list_containers_json()?;
    let mut out = String::new();
    for container in containers {
        if normalize_state(
            &json_string_any(&container, &["STATE", "State", "state", "status"])
                .unwrap_or_default(),
        ) != "running"
        {
            continue;
        }

        let labels = container
            .pointer("/configuration/labels")
            .unwrap_or(&Value::Null);
        let belongs_to_project = labels
            .get("com.docker.compose.project")
            .or_else(|| labels.get("com.dip.project"))
            .and_then(Value::as_str)
            == Some(project);
        if !belongs_to_project {
            continue;
        }

        if let Some(id) = apple_container_id(&container) {
            out.push_str(&id);
            out.push('\n');
        }
    }
    Ok(out)
}

fn apple_container_ip(container_id: &str) -> Result<String> {
    let output = Command::new("container")
        .args(["inspect", container_id])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("container inspect failed for container {container_id}");
    }

    let json: Value = serde_json::from_slice(&output.stdout)?;
    json.as_array()
        .and_then(|items| items.first())
        .and_then(|container| container.get("networks"))
        .and_then(Value::as_array)
        .and_then(|nets| nets.first())
        .and_then(|net| net.get("ipv4Address"))
        .and_then(Value::as_str)
        .and_then(|ip| ip.split('/').next())
        .filter(|ip| !ip.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("Could not find IP for container {container_id}"))
}

fn apple_project_containers(
    ctx: &BackendCtx,
    include_stopped: bool,
) -> Result<Vec<RuntimeProjectContainer>> {
    let containers = list_containers_json()?;
    let mut rows = Vec::new();

    for container in containers {
        if !include_stopped
            && normalize_state(
                &json_string_any(&container, &["STATE", "State", "state", "status"])
                    .unwrap_or_default(),
            ) != "running"
        {
            continue;
        }

        let labels = container
            .pointer("/configuration/labels")
            .unwrap_or(&Value::Null);
        let belongs_to_project = labels
            .get("com.docker.compose.project")
            .or_else(|| labels.get("com.dip.project"))
            .and_then(Value::as_str)
            == Some(ctx.project.project_name.as_str());
        if !belongs_to_project {
            continue;
        }

        let ip = container
            .get("networks")
            .and_then(Value::as_array)
            .and_then(|nets| nets.first())
            .and_then(|net| net.get("ipv4Address"))
            .and_then(Value::as_str)
            .and_then(|ip| ip.split('/').next())
            .filter(|ip| !ip.is_empty())
            .map(str::to_string);

        rows.push(RuntimeProjectContainer {
            labels: labels.clone(),
            ip,
        });
    }

    Ok(rows)
}

fn apple_running_count_by_project(project_name: &str) -> usize {
    let Ok(containers) = list_containers_json() else {
        return 0;
    };

    containers
        .into_iter()
        .filter(|container| {
            normalize_state(
                &json_string_any(container, &["STATE", "State", "state", "status"])
                    .unwrap_or_default(),
            ) == "running"
        })
        .filter(|container| {
            let labels = container
                .pointer("/configuration/labels")
                .unwrap_or(&Value::Null);
            labels
                .get("com.docker.compose.project")
                .or_else(|| labels.get("com.dip.project"))
                .and_then(Value::as_str)
                == Some(project_name)
        })
        .count()
}

fn json_string_any(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(value) = value.get(*key).and_then(value_to_string) {
            return Some(value);
        }
    }
    None
}

fn apple_container_id(value: &Value) -> Option<String> {
    json_string_any(value, &["ID", "Id", "id", "name", "Name"])
        .or_else(|| value.pointer("/configuration/id").and_then(value_to_string))
}

fn normalize_state(raw: &str) -> String {
    let raw = raw.to_ascii_lowercase();
    if raw.contains("running") {
        "running".to_string()
    } else if raw.contains("stopped") || raw.contains("exited") {
        "exited".to_string()
    } else {
        raw
    }
}

fn run_container_owned(ctx: &BackendCtx, args: &[String]) -> Result<()> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    log_container_cmd(ctx.verbose, &refs);
    let status = command_owned(args)
        .envs(ctx.project.get_env())
        .status()
        .context("failed to run container command")?;
    let code = status.code().unwrap_or(0);
    if !status.success() && code != 130 {
        anyhow::bail!("container command failed (exit {})", status);
    }
    Ok(())
}

fn run_container_status(args: &[&str], verbose: bool) -> Result<()> {
    log_container_cmd(verbose, args);
    let status = Command::new("container").args(args).status()?;
    let code = status.code().unwrap_or(0);
    if !status.success() && code != 130 {
        anyhow::bail!("container command failed (exit {})", status);
    }
    Ok(())
}

fn run_container_capture(args: &[&str], verbose: bool) -> std::result::Result<(), String> {
    log_container_cmd(verbose, args);
    let output = Command::new("container")
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }

    Err(command_output_message(&output.stdout, &output.stderr))
}

fn run_container_owned_capture(
    ctx: &BackendCtx,
    args: &[String],
) -> std::result::Result<(), String> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    log_container_cmd(ctx.verbose, &refs);
    let output = command_owned(args)
        .envs(ctx.project.get_env())
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }

    Err(command_output_message(&output.stdout, &output.stderr))
}

fn command_output_message(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    let message = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if message.is_empty() {
        "container command failed".to_string()
    } else {
        message
    }
}

fn stream_container_output(ctx: &BackendCtx, args: &[String], keywords: &[&str]) -> Result<()> {
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    log_container_cmd(ctx.verbose, &refs);
    let mut child = command_owned(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("failed to stream container command")?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("stdout not captured"))?;

    for line in std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
    {
        if keywords.is_empty()
            || keywords
                .iter()
                .any(|kw| line.to_ascii_lowercase().contains(&kw.to_ascii_lowercase()))
        {
            println!("{}", colorize_compose_line(&line, ctx.no_color));
        }
    }

    let status = child.wait()?;
    let code = status.code().unwrap_or(0);
    if !status.success() && code != 130 {
        anyhow::bail!("container command failed (exit {})", status);
    }
    Ok(())
}

fn command_owned(args: &[String]) -> Command {
    let mut command = Command::new("container");
    command.args(args);
    command
}

fn apple_bench(spec: &RuntimeBenchSpec) -> Result<RuntimeBenchMeasurement> {
    ensure_apple_image(&spec.image)?;
    let script = bench_disk_script(&spec.path, spec.size_mb);
    let mut totals = RuntimeBenchTotals::default();
    let runs = spec.warmup + spec.iterations.max(1);

    for index in 0..runs {
        let measured = index >= spec.warmup;
        let name = bench_container_name("apple");
        let _ = container_status(&["delete", "--force", &name]);

        let result = (|| -> Result<()> {
            let started = Instant::now();
            let args = apple_run_args(&name, spec);
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            container_status(&arg_refs)?;
            let start_elapsed = started.elapsed();

            let exec_started = Instant::now();
            container_status(&["exec", &name, "true"])?;
            let exec_elapsed = exec_started.elapsed();

            let disk_started = Instant::now();
            container_status(&["exec", &name, "sh", "-lc", &script])?;
            let disk_elapsed = disk_started.elapsed();

            if measured {
                totals.start += start_elapsed;
                totals.exec += exec_elapsed;
                totals.disk += disk_elapsed;
            }
            Ok(())
        })();

        let _ = container_status(&["delete", "--force", &name]);
        result?;
    }

    Ok(bench_measurement("apple", spec, totals))
}

fn apple_bench_steady(spec: &RuntimeBenchSpec) -> Result<RuntimeBenchMeasurement> {
    ensure_apple_image(&spec.image)?;
    let script = bench_disk_script(&spec.path, spec.size_mb);
    let mut totals = RuntimeBenchTotals::default();
    let runs = spec.warmup + spec.iterations.max(1);
    let name = bench_container_name("apple");
    let _ = container_status(&["delete", "--force", &name]);

    let result = (|| -> Result<()> {
        let started = Instant::now();
        let args = apple_run_args(&name, spec);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        container_status(&arg_refs)?;
        totals.start = started.elapsed();

        for index in 0..runs {
            let measured = index >= spec.warmup;

            let exec_started = Instant::now();
            container_status(&["exec", &name, "true"])?;
            let exec_elapsed = exec_started.elapsed();

            let disk_started = Instant::now();
            container_status(&["exec", &name, "sh", "-lc", &script])?;
            let disk_elapsed = disk_started.elapsed();

            if measured {
                totals.exec += exec_elapsed;
                totals.disk += disk_elapsed;
            }
        }

        Ok(())
    })();

    let _ = container_status(&["delete", "--force", &name]);
    result?;

    Ok(steady_bench_measurement("apple", spec, totals))
}

fn apple_run_args(name: &str, spec: &RuntimeBenchSpec) -> Vec<String> {
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

fn ensure_apple_image(image: &str) -> Result<()> {
    if container_status(&["image", "inspect", image]).is_ok() {
        return Ok(());
    }
    container_status(&["image", "pull", image])
}

fn container_status(args: &[&str]) -> Result<()> {
    let output = Command::new("container").args(args).output()?;
    if output.status.success() {
        return Ok(());
    }

    let message = command_output_message(&output.stdout, &output.stderr);
    anyhow::bail!("container {} failed: {}", args.join(" "), message)
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

fn apple_system_info() -> Result<RuntimeSystemInfo> {
    let version_raw = Command::new("container")
        .args(["system", "version"])
        .output()?;
    Ok(RuntimeSystemInfo {
        version_label: "Apple Container:",
        version: apple_container_version(&String::from_utf8_lossy(&version_raw.stdout)),
        images: None,
        containers: None,
    })
}

fn apple_container_version(raw: &str) -> String {
    raw.lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            match (parts.next(), parts.next()) {
                (Some("container"), Some(version)) => Some(version.to_string()),
                _ => None,
            }
        })
        .unwrap_or_else(|| raw.lines().next().unwrap_or("").trim().to_string())
}

fn log_container_cmd(verbose: bool, args: &[&str]) {
    log_verbose(verbose, &format!("  container {}", args.join(" ")));
}

fn unsupported_compose<T>(args: &[&str]) -> Result<T> {
    anyhow::bail!(
        "Apple Container runtime does not support this compose operation yet: {}",
        args.join(" ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_stable_and_cli_safe() {
        assert_eq!(sanitize_name("Newline Backend"), "newline-backend");
        assert_eq!(sanitize_name("app_api"), "app_api");
    }

    #[test]
    fn command_string_runs_through_shell() {
        assert_eq!(
            command_args(&Value::String("pnpm dev:web".to_string())),
            vec!["/bin/sh", "-lc", "pnpm dev:web"]
        );
    }

    #[test]
    fn volume_spec_preserves_read_only_flag() {
        let volume = compose_file::VolumeConfig {
            kind: Some("bind".to_string()),
            source: Some("/host".to_string()),
            target: Some("/app".to_string()),
            read_only: Some(true),
        };
        assert_eq!(volume_spec(&volume).as_deref(), Some("/host:/app:ro"));
    }

    #[test]
    fn empty_inspect_array_is_not_an_existing_resource() {
        assert!(!json_output_has_entries(b"[]"));
        assert!(json_output_has_entries(br#"[{"id":"default"}]"#));
    }

    #[test]
    fn run_network_can_be_replaced_for_retry() {
        let mut args = vec![
            "run".to_string(),
            "--network".to_string(),
            "dip-nextjs".to_string(),
            "postgres:alpine".to_string(),
        ];

        replace_run_network(&mut args, "default");

        assert_eq!(args[2], "default");
    }

    #[test]
    fn apple_container_id_reads_nested_configuration_id() {
        let value = serde_json::json!({
            "status": "running",
            "configuration": {
                "id": "dip-nextjs-app"
            }
        });

        assert_eq!(
            apple_container_id(&value).as_deref(),
            Some("dip-nextjs-app")
        );
    }

    #[test]
    fn exec_parser_keeps_env_options_before_service() {
        let args = ["exec", "-e", "PGPASSWORD=secret", "-it", "db", "psql"];
        let parsed = parse_exec_service(&args).unwrap();

        assert_eq!(parsed.service, "db");
        assert_eq!(parsed.command_start, 5);
        assert_eq!(
            parsed.options,
            vec![
                "--env".to_string(),
                "PGPASSWORD=secret".to_string(),
                "--interactive".to_string(),
                "--tty".to_string(),
            ]
        );
    }

    #[test]
    fn healthcheck_cmd_shell_is_translated_to_shell_exec() {
        let service = ServiceConfig {
            build: None,
            image: None,
            labels: Value::Null,
            volumes: vec![],
            ports: Value::Null,
            environment: Value::Null,
            env_file: Value::Null,
            command: Value::Null,
            entrypoint: Value::Null,
            working_dir: None,
            depends_on: vec![],
            healthcheck: serde_json::json!({
                "test": ["CMD-SHELL", "pg_isready -U nextjs -d nextjs"]
            }),
        };

        assert_eq!(
            healthcheck_command_args(&service).unwrap(),
            vec!["sh", "-c", "pg_isready -U nextjs -d nextjs"]
        );
    }

    #[test]
    fn parses_apple_container_version_table() {
        let raw = "COMPONENT VERSION BUILD COMMIT\ncontainer 0.12.3 release unspecified\n";
        assert_eq!(apple_container_version(raw), "0.12.3");
    }
}
