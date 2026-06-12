use std::path::Path;

use crate::utils::style::Stylize;
use anyhow::Result;

use crate::commands::compose_config::{self, BuildConfig};
use crate::commands::ctx::Ctx;
use crate::project::ProjectConfig;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Level {
    Ok,
    Warn,
    Fail,
}

struct Check {
    level: Level,
    label: String,
    message: String,
    hint: Option<String>,
}

impl Check {
    fn ok(label: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            level: Level::Ok,
            label: label.into(),
            message: message.into(),
            hint: None,
        }
    }

    fn warn(
        label: impl Into<String>,
        message: impl Into<String>,
        hint: impl Into<Option<String>>,
    ) -> Self {
        Self {
            level: Level::Warn,
            label: label.into(),
            message: message.into(),
            hint: hint.into(),
        }
    }

    fn fail(
        label: impl Into<String>,
        message: impl Into<String>,
        hint: impl Into<Option<String>>,
    ) -> Self {
        Self {
            level: Level::Fail,
            label: label.into(),
            message: message.into(),
            hint: hint.into(),
        }
    }
}

pub fn run(fix: bool, verbose: bool, no_color: bool) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;
    if fix {
        let applied = apply_fixes(&ctx)?;
        ctx.out.section("validate --fix", || {
            if applied.is_empty() {
                println!("  {}", "No safe fixes found".dimmed());
            } else {
                for action in &applied {
                    println!("  {} {}", "✓".green(), action);
                }
            }
        });
    }

    let config = compose_config::load(&ctx.rt)?;
    let project = &ctx.rt.project;

    let mut checks = vec![
        check_file("compose file", &project.compose_file),
        check_file("env file", &project.env_file),
        check_file("default env", &project.dip_dir.join("default.env")),
    ];

    if config.services.is_empty() {
        checks.push(Check::fail("compose services", "no services defined", None));
    }

    for (service_name, service) in &config.services {
        match &service.build {
            Some(build) => add_build_checks(project, service_name, build, &mut checks),
            None if service.image.is_none() => checks.push(Check::warn(
                format!("{service_name}.image"),
                "service has neither image nor build",
                None,
            )),
            None => {}
        }

        for volume in &service.volumes {
            if volume.kind.as_deref() != Some("bind") {
                continue;
            }
            let Some(source) = volume.source.as_deref() else {
                continue;
            };
            if source.is_empty() {
                checks.push(Check::fail(
                    format!("{service_name}.volumes"),
                    "bind mount has an empty source path",
                    None,
                ));
            } else if !Path::new(source).exists() {
                checks.push(Check::warn(
                    format!("{service_name}.volumes"),
                    format!(
                        "{} does not exist{}",
                        source,
                        volume
                            .target
                            .as_deref()
                            .map(|t| format!(" (mounted at {t})"))
                            .unwrap_or_default()
                    ),
                    None,
                ));
            }
        }

        add_label_checks(service_name, &service.label_entries(), &mut checks);
    }

    let failures = checks.iter().filter(|c| c.level == Level::Fail).count();
    let warnings = checks.iter().filter(|c| c.level == Level::Warn).count();

    ctx.out.section("validate", || {
        let width = checks.iter().map(|c| c.label.len()).max().unwrap_or(0) + 2;
        for check in &checks {
            let icon = match check.level {
                Level::Ok => "✓".green(),
                Level::Warn => "⚠".yellow(),
                Level::Fail => "✗".red().bold(),
            };
            println!(
                "  {icon} {:<width$} {}",
                format!("{}:", check.label).dimmed(),
                check.message,
                width = width
            );
            if let Some(hint) = &check.hint {
                println!("    {} {}", "→".dimmed(), hint.dimmed());
            }
        }

        println!();
        if failures > 0 {
            println!(
                "  {} {} failed, {} warning(s)",
                "✗".red().bold(),
                failures,
                warnings
            );
        } else if warnings > 0 {
            println!(
                "  {} {} warning(s) — no blocking issues",
                "⚠".yellow(),
                warnings
            );
        } else {
            println!("  {} All checks passed", "✓".green().bold());
        }
    });

    if failures > 0 {
        anyhow::bail!("validation failed");
    }

    Ok(())
}

fn apply_fixes(ctx: &Ctx) -> Result<Vec<String>> {
    let mut applied = Vec::new();

    let config = compose_config::load(&ctx.rt)?;
    let compose_actions = collect_compose_context_fixes(&ctx.rt.project, &config);
    if !compose_actions.is_empty() {
        let content = std::fs::read_to_string(&ctx.rt.project.compose_file)?;
        let (updated, changed) = apply_compose_context_fixes(&content, &compose_actions);
        if changed {
            std::fs::write(&ctx.rt.project.compose_file, updated)?;
            for action in &compose_actions {
                applied.push(format!(
                    "{}: set build.context to `{}`",
                    action.service, action.new_context
                ));
            }
        }
    }

    let config = compose_config::load(&ctx.rt)?;
    for (service_name, service) in &config.services {
        let Some(build) = &service.build else {
            continue;
        };
        let Some(target) = build.target.as_deref() else {
            continue;
        };
        let Some(dockerfile) = build.dockerfile_path() else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(&dockerfile) else {
            continue;
        };
        let stages = compose_config::dockerfile_stages(&content);
        if stages.contains(target) || !stages.is_empty() || !is_safe_stage_name(target) {
            continue;
        }

        if let Some(updated) = add_stage_alias_to_first_from(&content, target) {
            std::fs::write(&dockerfile, updated)?;
            applied.push(format!(
                "{}: added `AS {}` to {}",
                service_name,
                target,
                dockerfile.display()
            ));
        }
    }

    Ok(applied)
}

struct ComposeContextFix {
    service: String,
    new_context: &'static str,
}

fn collect_compose_context_fixes(
    project: &ProjectConfig,
    config: &compose_config::ComposeConfig,
) -> Vec<ComposeContextFix> {
    let mut fixes = Vec::new();
    let dip_dockerfile = project.dip_dir.join("Dockerfile");
    if !dip_dockerfile.is_file() {
        return fixes;
    }

    for (service_name, service) in &config.services {
        let Some(build) = &service.build else {
            continue;
        };
        if build.dockerfile_name() != "Dockerfile" {
            continue;
        }
        let Some(path) = build.dockerfile_path() else {
            continue;
        };
        if path.is_file() {
            continue;
        }
        if !path.ends_with("Dockerfile") || path.parent() != Some(project.root_dir.as_path()) {
            continue;
        }

        fixes.push(ComposeContextFix {
            service: service_name.clone(),
            new_context: ".",
        });
    }

    fixes
}

fn apply_compose_context_fixes(content: &str, fixes: &[ComposeContextFix]) -> (String, bool) {
    let fixes: std::collections::HashMap<&str, &str> = fixes
        .iter()
        .map(|fix| (fix.service.as_str(), fix.new_context))
        .collect();
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let had_trailing_newline = content.ends_with('\n');
    let mut changed = false;
    let mut services_indent: Option<usize> = None;
    let mut service_indent: Option<usize> = None;
    let mut current_service: Option<(String, usize)> = None;
    let mut build_indent: Option<usize> = None;

    for line in &mut lines {
        let Some((indent, key, value)) = yaml_key(line) else {
            continue;
        };

        if key == "services" && value.trim().is_empty() {
            services_indent = Some(indent);
            service_indent = None;
            current_service = None;
            build_indent = None;
            continue;
        }

        let Some(active_services_indent) = services_indent else {
            continue;
        };

        if indent <= active_services_indent {
            services_indent = None;
            service_indent = None;
            current_service = None;
            build_indent = None;
            continue;
        }

        let active_service_indent = *service_indent.get_or_insert(indent);
        if let Some((_, service_indent)) = &current_service
            && indent <= *service_indent
        {
            current_service = None;
            build_indent = None;
        }
        if let Some(active_build_indent) = build_indent
            && indent <= active_build_indent
        {
            build_indent = None;
        }

        if indent == active_service_indent {
            current_service = fixes.get(key).map(|_| (key.to_string(), indent));
            build_indent = None;
            continue;
        }

        if current_service.is_some() && key == "build" && value.trim().is_empty() {
            build_indent = Some(indent);
            continue;
        }

        if current_service.is_some()
            && build_indent.is_some()
            && key == "context"
            && let Some(new_context) = current_service
                .as_ref()
                .and_then(|(service, _)| fixes.get(service.as_str()))
            && value.trim() != *new_context
        {
            *line = replace_yaml_scalar(line, new_context);
            changed = true;
        }
    }

    let mut updated = lines.join("\n");
    if had_trailing_newline {
        updated.push('\n');
    }
    (updated, changed)
}

fn yaml_key(line: &str) -> Option<(usize, &str, &str)> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let indent = line.len() - trimmed.len();
    let (key, value) = trimmed.split_once(':')?;
    let key = key.trim();
    if key.is_empty() || key.starts_with('-') || key.contains([' ', '{', '}', '[', ']']) {
        return None;
    }
    Some((indent, key.trim_matches('"').trim_matches('\''), value))
}

fn replace_yaml_scalar(line: &str, new_value: &str) -> String {
    let Some((left, right)) = line.split_once(':') else {
        return line.to_string();
    };
    let comment = right.find('#').map(|idx| right[idx..].trim_end());
    match comment {
        Some(comment) => format!("{left}: {new_value}  {comment}"),
        None => format!("{left}: {new_value}"),
    }
}

fn is_safe_stage_name(target: &str) -> bool {
    !target.is_empty()
        && target
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

fn add_stage_alias_to_first_from(content: &str, target: &str) -> Option<String> {
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();
    let had_trailing_newline = content.ends_with('\n');

    for line in &mut lines {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let first = trimmed.split_whitespace().next()?;
        if !first.eq_ignore_ascii_case("FROM") {
            continue;
        }
        if line_has_from_alias(trimmed) {
            return None;
        }

        *line = add_alias_before_comment(line, target);
        let mut updated = lines.join("\n");
        if had_trailing_newline {
            updated.push('\n');
        }
        return Some(updated);
    }

    None
}

fn line_has_from_alias(line: &str) -> bool {
    let mut parts = line.split_whitespace();
    while let Some(part) = parts.next() {
        if part.starts_with('#') {
            break;
        }
        if part.eq_ignore_ascii_case("AS") && parts.next().is_some() {
            return true;
        }
    }
    false
}

fn add_alias_before_comment(line: &str, target: &str) -> String {
    if let Some(idx) = line.find(" #") {
        let (prefix, comment) = line.split_at(idx);
        format!("{} AS {}{}", prefix.trim_end(), target, comment)
    } else {
        format!("{} AS {}", line.trim_end(), target)
    }
}

fn check_file(label: &str, path: &Path) -> Check {
    if path.is_file() {
        Check::ok(label, format!("{} exists", path.display()))
    } else {
        Check::fail(label, format!("{} is missing", path.display()), None)
    }
}

fn add_build_checks(
    project: &ProjectConfig,
    service_name: &str,
    build: &BuildConfig,
    checks: &mut Vec<Check>,
) {
    let context = build.context_path();
    match &context {
        Some(path) if path.is_dir() => checks.push(Check::ok(
            format!("{service_name}.build.context"),
            format!("{} exists", path.display()),
        )),
        Some(path) => checks.push(Check::fail(
            format!("{service_name}.build.context"),
            format!("{} is missing", path.display()),
            None,
        )),
        None => checks.push(Check::fail(
            format!("{service_name}.build.context"),
            "build context is missing",
            None,
        )),
    }

    let dockerfile_path = build.dockerfile_path();
    match &dockerfile_path {
        Some(path) if path.is_file() => checks.push(Check::ok(
            format!("{service_name}.build.dockerfile"),
            format!("{} exists", path.display()),
        )),
        Some(path) => checks.push(Check::fail(
            format!("{service_name}.build.dockerfile"),
            format!("{} is missing", path.display()),
            dockerfile_hint(project, path),
        )),
        None => checks.push(Check::fail(
            format!("{service_name}.build.dockerfile"),
            "dockerfile path could not be resolved",
            None,
        )),
    }

    if let Some(target) = build.target.as_deref() {
        match dockerfile_path
            .as_deref()
            .and_then(|path| std::fs::read_to_string(path).ok())
        {
            Some(content) => {
                let stages = compose_config::dockerfile_stages(&content);
                if stages.contains(target) {
                    checks.push(Check::ok(
                        format!("{service_name}.build.target"),
                        format!("stage '{target}' exists"),
                    ));
                } else {
                    let known = if stages.is_empty() {
                        "no named stages found".to_string()
                    } else {
                        format!(
                            "known stages: {}",
                            stages.into_iter().collect::<Vec<_>>().join(", ")
                        )
                    };
                    checks.push(Check::fail(
                        format!("{service_name}.build.target"),
                        format!("stage '{target}' is missing ({known})"),
                        Some(
                            "remove build.target or add `AS <target>` to the Dockerfile"
                                .to_string(),
                        ),
                    ));
                }
            }
            None => checks.push(Check::warn(
                format!("{service_name}.build.target"),
                format!("cannot verify target '{target}' because Dockerfile is missing"),
                None,
            )),
        }
    }
}

fn dockerfile_hint(project: &ProjectConfig, missing_path: &Path) -> Option<String> {
    let dip_dockerfile = project.dip_dir.join("Dockerfile");
    if dip_dockerfile.is_file() {
        return Some(format!(
            "found {}; if this compose file lives in .dip, use `context: .` and `dockerfile: Dockerfile`",
            dip_dockerfile.display()
        ));
    }

    let parent = missing_path.parent().unwrap_or_else(|| Path::new("."));
    let nearby = parent.join(".dip").join("Dockerfile");
    if nearby.is_file() {
        return Some(format!(
            "found {}; update build.dockerfile or build.context",
            nearby.display()
        ));
    }

    None
}

fn add_label_checks(service_name: &str, labels: &[(String, String)], checks: &mut Vec<Check>) {
    for (key, value) in labels {
        if key != "dip.host" && !key.starts_with("dip.host.") {
            continue;
        }

        match validate_host_label(value) {
            Ok((domain, port)) => checks.push(Check::ok(
                format!("{service_name}.{key}"),
                format!("{domain}:{port}"),
            )),
            Err(message) => checks.push(Check::fail(
                format!("{service_name}.{key}"),
                message,
                Some("expected format: domain.test:80".to_string()),
            )),
        }
    }
}

fn validate_host_label(raw: &str) -> std::result::Result<(String, u16), String> {
    let raw = raw.trim().trim_matches('"').trim_matches('\'').trim();
    if raw.is_empty() {
        return Err("empty dip.host label".to_string());
    }

    let (domain, port) = match raw.rsplit_once(':') {
        Some((domain, port)) => {
            let port = port
                .parse::<u16>()
                .map_err(|_| format!("invalid port in dip.host label: {raw}"))?;
            (domain.trim(), port)
        }
        None => (raw, 80),
    };

    if domain.is_empty() {
        return Err("empty domain in dip.host label".to_string());
    }

    Ok((domain.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_context_fix_rewrites_only_target_service_build_context() {
        let input = "\
services:
  app:
    build:
      context: ..
      dockerfile: Dockerfile
  db:
    image: mysql
";
        let fixes = vec![ComposeContextFix {
            service: "app".to_string(),
            new_context: ".",
        }];

        let (updated, changed) = apply_compose_context_fixes(input, &fixes);

        assert!(changed);
        assert!(updated.contains("      context: .\n"));
        assert!(updated.contains("  db:\n    image: mysql\n"));
    }

    #[test]
    fn compose_context_fix_stays_inside_services_block() {
        let input = "\
services:
  db:
    environment:
      app: nope
app:
  build:
    context: ..
";
        let fixes = vec![ComposeContextFix {
            service: "app".to_string(),
            new_context: ".",
        }];

        let (updated, changed) = apply_compose_context_fixes(input, &fixes);

        assert!(!changed);
        assert_eq!(updated, input);
    }

    #[test]
    fn stage_alias_added_before_comment() {
        let input = "FROM php:8.4-fpm # base image\nRUN true\n";
        let updated = add_stage_alias_to_first_from(input, "dev").unwrap();

        assert_eq!(updated, "FROM php:8.4-fpm AS dev # base image\nRUN true\n");
    }

    #[test]
    fn host_label_defaults_to_port_80() {
        assert_eq!(
            validate_host_label("api.example.test").unwrap(),
            ("api.example.test".to_string(), 80)
        );
    }

    #[test]
    fn host_label_rejects_invalid_port() {
        assert!(validate_host_label("api.example.test:nope").is_err());
    }
}
