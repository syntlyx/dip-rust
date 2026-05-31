#[cfg(any(target_os = "macos", test))]
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
#[cfg(any(target_os = "macos", test))]
use std::path::{Component, Path};

#[cfg(target_os = "macos")]
use anyhow::Context;
#[cfg(any(target_os = "macos", test))]
use anyhow::Result;
use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "macos", test))]
use serde_json::Map;
use serde_json::Value;

#[cfg(target_os = "macos")]
use crate::project::ProjectConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComposeConfig {
    #[serde(default)]
    pub services: BTreeMap<String, ServiceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    #[serde(default)]
    pub build: Option<BuildConfig>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub labels: Value,
    #[serde(default)]
    pub volumes: Vec<VolumeConfig>,
    #[serde(default)]
    pub ports: Value,
    #[serde(default)]
    pub environment: Value,
    #[serde(default)]
    pub env_file: Value,
    #[serde(default)]
    pub command: Value,
    #[serde(default)]
    pub entrypoint: Value,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub healthcheck: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildConfig {
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub dockerfile: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub args: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeConfig {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub read_only: Option<bool>,
}

impl BuildConfig {
    pub fn context_path(&self) -> Option<PathBuf> {
        self.context.as_ref().map(PathBuf::from)
    }

    pub fn dockerfile_name(&self) -> &str {
        self.dockerfile.as_deref().unwrap_or("Dockerfile")
    }

    pub fn dockerfile_path(&self) -> Option<PathBuf> {
        let context = self.context_path()?;
        let dockerfile = PathBuf::from(self.dockerfile_name());
        Some(if dockerfile.is_absolute() {
            dockerfile
        } else {
            context.join(dockerfile)
        })
    }
}

impl ServiceConfig {
    pub fn label_entries(&self) -> Vec<(String, String)> {
        match &self.labels {
            Value::Object(map) => map
                .iter()
                .filter_map(|(key, value)| {
                    let value = scalar_to_string(value)?.trim().to_string();
                    Some((key.clone(), value))
                })
                .collect(),
            Value::Array(items) => items
                .iter()
                .filter_map(|item| {
                    let raw = item.as_str()?.trim();
                    let (key, value) = raw.split_once('=')?;
                    Some((key.trim().to_string(), value.trim().to_string()))
                })
                .collect(),
            _ => vec![],
        }
    }
}

#[cfg(target_os = "macos")]
pub fn load_project_compose(project: &ProjectConfig) -> Result<ComposeConfig> {
    let content = std::fs::read_to_string(&project.compose_file)
        .with_context(|| format!("failed to read {}", project.compose_file.display()))?;
    let value: Value = noyalib::from_str(&content)
        .with_context(|| format!("failed to parse {}", project.compose_file.display()))?;
    let value = interpolate_value(value, &project.get_env());
    let base_dir = project
        .compose_file
        .parent()
        .unwrap_or_else(|| Path::new("."));
    parse_compose_value(value, base_dir)
}

pub fn dockerfile_stages(content: &str) -> BTreeSet<String> {
    let mut stages = BTreeSet::new();
    for line in content.lines() {
        let line = line.trim_start();
        if line.starts_with('#') {
            continue;
        }

        let mut parts = line.split_whitespace();
        let Some(first) = parts.next() else {
            continue;
        };
        if !first.eq_ignore_ascii_case("FROM") {
            continue;
        }

        while let Some(part) = parts.next() {
            if part.eq_ignore_ascii_case("AS") {
                if let Some(stage) = parts.next() {
                    stages.insert(strip_stage_quotes(stage).to_string());
                }
                break;
            }
        }
    }
    stages
}

#[cfg(any(target_os = "macos", test))]
fn parse_compose_value(value: Value, base_dir: &Path) -> Result<ComposeConfig> {
    let services = value
        .get("services")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("compose file has no services map"))?;

    let mut parsed = BTreeMap::new();
    for (name, service_value) in services {
        if let Some(service) = parse_service(service_value, base_dir) {
            parsed.insert(name.clone(), service);
        }
    }

    Ok(ComposeConfig { services: parsed })
}

#[cfg(any(target_os = "macos", test))]
fn parse_service(value: &Value, base_dir: &Path) -> Option<ServiceConfig> {
    let service = merge_service_map(value)?;

    Some(ServiceConfig {
        build: service.get("build").and_then(|v| parse_build(v, base_dir)),
        image: service
            .get("image")
            .and_then(scalar_to_string)
            .map(str::to_string),
        labels: service
            .get("labels")
            .map(parse_labels)
            .unwrap_or(Value::Object(Map::new())),
        volumes: service
            .get("volumes")
            .map(|v| parse_volumes(v, base_dir))
            .unwrap_or_default(),
        ports: service
            .get("ports")
            .map(parse_string_list_value)
            .unwrap_or(Value::Array(vec![])),
        environment: service
            .get("environment")
            .map(parse_environment)
            .unwrap_or(Value::Object(Map::new())),
        env_file: service
            .get("env_file")
            .map(|v| parse_path_list_value(v, base_dir))
            .unwrap_or(Value::Array(vec![])),
        command: service.get("command").cloned().unwrap_or(Value::Null),
        entrypoint: service.get("entrypoint").cloned().unwrap_or(Value::Null),
        working_dir: service
            .get("working_dir")
            .or_else(|| service.get("workdir"))
            .and_then(scalar_to_string)
            .map(str::to_string),
        depends_on: service
            .get("depends_on")
            .map(parse_depends_on)
            .unwrap_or_default(),
        healthcheck: service.get("healthcheck").cloned().unwrap_or(Value::Null),
    })
}

#[cfg(any(target_os = "macos", test))]
fn merge_service_map(value: &Value) -> Option<Map<String, Value>> {
    let map = value.as_object()?;
    let mut merged = Map::new();

    if let Some(base) = map.get("<<") {
        merge_value_into(&mut merged, base);
    }

    for (key, value) in map {
        if key != "<<" {
            merged.insert(key.clone(), value.clone());
        }
    }

    Some(merged)
}

#[cfg(any(target_os = "macos", test))]
fn merge_value_into(target: &mut Map<String, Value>, value: &Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                target.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
        Value::Array(items) => {
            for item in items {
                merge_value_into(target, item);
            }
        }
        _ => {}
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_build(value: &Value, base_dir: &Path) -> Option<BuildConfig> {
    match value {
        Value::String(context) => Some(BuildConfig {
            context: Some(resolve_path_string(base_dir, context)),
            dockerfile: None,
            target: None,
            args: BTreeMap::new(),
        }),
        Value::Object(map) => {
            let context = map
                .get("context")
                .and_then(scalar_to_string)
                .map(|context| resolve_path_string(base_dir, context))
                .unwrap_or_else(|| base_dir.to_string_lossy().into_owned());
            let args = map
                .get("args")
                .and_then(Value::as_object)
                .map(|args| {
                    args.iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            Some(BuildConfig {
                context: Some(context),
                dockerfile: map
                    .get("dockerfile")
                    .and_then(scalar_to_string)
                    .map(str::to_string),
                target: map
                    .get("target")
                    .and_then(scalar_to_string)
                    .map(str::to_string),
                args,
            })
        }
        _ => None,
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_labels(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut labels = Map::new();
            for (key, value) in map {
                if let Some(value) = scalar_to_string(value) {
                    labels.insert(key.clone(), Value::String(value.to_string()));
                }
            }
            Value::Object(labels)
        }
        Value::Array(items) => {
            let mut labels = Map::new();
            for item in items {
                let Some(raw) = item.as_str() else {
                    continue;
                };
                if let Some((key, value)) = raw.split_once('=') {
                    labels.insert(
                        key.trim().to_string(),
                        Value::String(value.trim().to_string()),
                    );
                }
            }
            Value::Object(labels)
        }
        _ => Value::Object(Map::new()),
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_environment(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut env = Map::new();
            for (key, value) in map {
                if let Some(value) = scalar_to_string(value) {
                    env.insert(key.clone(), Value::String(value.to_string()));
                }
            }
            Value::Object(env)
        }
        Value::Array(items) => {
            let mut env = Map::new();
            for item in items {
                let Some(raw) = item.as_str() else {
                    continue;
                };
                if let Some((key, value)) = raw.split_once('=') {
                    env.insert(
                        key.trim().to_string(),
                        Value::String(value.trim().to_string()),
                    );
                }
            }
            Value::Object(env)
        }
        _ => Value::Object(Map::new()),
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_string_list_value(value: &Value) -> Value {
    match value {
        Value::String(raw) => Value::Array(vec![Value::String(raw.clone())]),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .filter_map(scalar_to_string)
                .map(|s| Value::String(s.to_string()))
                .collect(),
        ),
        _ => Value::Array(vec![]),
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_path_list_value(value: &Value, base_dir: &Path) -> Value {
    match parse_string_list_value(value) {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(|path| Value::String(resolve_path_string(base_dir, path)))
                .collect(),
        ),
        _ => Value::Array(vec![]),
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_depends_on(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(scalar_to_string)
            .map(str::to_string)
            .collect(),
        Value::Object(map) => map.keys().cloned().collect(),
        _ => vec![],
    }
}

#[cfg(any(target_os = "macos", test))]
fn parse_volumes(value: &Value, base_dir: &Path) -> Vec<VolumeConfig> {
    let Some(items) = value.as_array() else {
        return vec![];
    };

    items
        .iter()
        .filter_map(|item| parse_volume(item, base_dir))
        .collect()
}

#[cfg(any(target_os = "macos", test))]
fn parse_volume(value: &Value, base_dir: &Path) -> Option<VolumeConfig> {
    if let Some(raw) = value.as_str() {
        return parse_short_volume(raw, base_dir);
    }

    let map = value.as_object()?;
    let kind = map
        .get("type")
        .and_then(scalar_to_string)
        .unwrap_or("volume")
        .to_string();
    let source = map
        .get("source")
        .or_else(|| map.get("src"))
        .and_then(scalar_to_string)
        .map(|source| {
            if kind == "bind" {
                resolve_path_string(base_dir, source)
            } else {
                source.to_string()
            }
        });
    let target = map
        .get("target")
        .or_else(|| map.get("dst"))
        .or_else(|| map.get("destination"))
        .and_then(scalar_to_string)
        .map(str::to_string);
    let read_only = map
        .get("read_only")
        .or_else(|| map.get("readonly"))
        .and_then(Value::as_bool);

    Some(VolumeConfig {
        kind: Some(kind),
        source,
        target,
        read_only,
    })
}

#[cfg(any(target_os = "macos", test))]
fn parse_short_volume(raw: &str, base_dir: &Path) -> Option<VolumeConfig> {
    let parts: Vec<&str> = raw.split(':').collect();
    match parts.as_slice() {
        [target] => Some(VolumeConfig {
            kind: Some("volume".to_string()),
            source: None,
            target: Some((*target).to_string()),
            read_only: None,
        }),
        [source, target] | [source, target, _] => {
            let kind = if is_bind_source(source) {
                "bind"
            } else {
                "volume"
            };
            let source = if kind == "bind" {
                resolve_path_string(base_dir, source)
            } else {
                (*source).to_string()
            };
            Some(VolumeConfig {
                kind: Some(kind.to_string()),
                source: Some(source),
                target: Some((*target).to_string()),
                read_only: parts.get(2).map(|mode| mode.contains("ro")),
            })
        }
        _ => None,
    }
}

#[cfg(any(target_os = "macos", test))]
fn is_bind_source(source: &str) -> bool {
    source.starts_with('/')
        || source.starts_with("./")
        || source.starts_with("../")
        || source.starts_with('~')
}

#[cfg(any(target_os = "macos", test))]
fn resolve_path_string(base_dir: &Path, path: &str) -> String {
    let path = PathBuf::from(path);
    let resolved = if path.is_absolute() {
        path
    } else {
        base_dir.join(path)
    };
    normalize_path(&resolved).to_string_lossy().into_owned()
}

#[cfg(any(target_os = "macos", test))]
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(any(target_os = "macos", test))]
fn interpolate_value(value: Value, env: &HashMap<String, String>) -> Value {
    match value {
        Value::String(s) => Value::String(interpolate_string(&s, env)),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(|v| interpolate_value(v, env))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, value)| (key, interpolate_value(value, env)))
                .collect(),
        ),
        other => other,
    }
}

#[cfg(any(target_os = "macos", test))]
fn interpolate_string(input: &str, env: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        rest = &rest[start + 2..];
        let Some(end) = rest.find('}') else {
            out.push_str("${");
            out.push_str(rest);
            return out;
        };

        let expr = &rest[..end];
        out.push_str(&resolve_env_expr(expr, env));
        rest = &rest[end + 1..];
    }

    out.push_str(rest);
    out
}

#[cfg(any(target_os = "macos", test))]
fn resolve_env_expr(expr: &str, env: &HashMap<String, String>) -> String {
    if let Some((key, default)) = expr.split_once(":-") {
        return match env.get(key) {
            Some(value) if !value.is_empty() => value.clone(),
            _ => default.to_string(),
        };
    }

    if let Some((key, default)) = expr.split_once('-') {
        return env.get(key).cloned().unwrap_or_else(|| default.to_string());
    }

    env.get(expr).cloned().unwrap_or_default()
}

fn scalar_to_string(value: &Value) -> Option<&str> {
    value.as_str()
}

fn strip_stage_quotes(stage: &str) -> &str {
    stage
        .trim_matches('"')
        .trim_matches('\'')
        .trim_end_matches('\r')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dockerfile_stages_parse_named_stages() {
        let content = r#"
FROM node:22 AS deps
FROM --platform=$BUILDPLATFORM rust:1 AS builder
FROM alpine
"#;

        let stages = dockerfile_stages(content);
        assert!(stages.contains("deps"));
        assert!(stages.contains("builder"));
        assert!(!stages.contains("alpine"));
    }

    #[test]
    fn parses_build_string_and_env_defaults() {
        let base = Path::new("/project/.dip");
        let value: Value = noyalib::from_str(
            r#"
services:
  app:
    build: .
    labels:
      dip.host: "${DOMAIN:-app.test}:80"
    volumes:
      - ${PROJECT_ROOT}:/app:ro
"#,
        )
        .unwrap();
        let mut env = HashMap::new();
        env.insert("PROJECT_ROOT".to_string(), "/project".to_string());
        let config = parse_compose_value(interpolate_value(value, &env), base).unwrap();
        let app = &config.services["app"];

        assert_eq!(
            app.build.as_ref().unwrap().context.as_deref(),
            Some("/project/.dip")
        );
        assert_eq!(app.label_entries()[0].1, "app.test:80");
        assert_eq!(app.volumes[0].kind.as_deref(), Some("bind"));
        assert_eq!(app.volumes[0].source.as_deref(), Some("/project"));
        assert_eq!(app.volumes[0].read_only, Some(true));
    }

    #[test]
    fn parses_merge_key_for_service_defaults() {
        let base = Path::new("/project/.dip");
        let value: Value = noyalib::from_str(
            r#"
x-app: &app
  build:
    context: .
  env_file:
    - ${DIP_DIR}/env/base.env
services:
  web:
    <<: *app
    image: app-web
"#,
        )
        .unwrap();
        let mut env = HashMap::new();
        env.insert("DIP_DIR".to_string(), "/project/.dip".to_string());
        let config = parse_compose_value(interpolate_value(value, &env), base).unwrap();
        let web = &config.services["web"];

        assert_eq!(
            web.build.as_ref().unwrap().context.as_deref(),
            Some("/project/.dip")
        );
        assert_eq!(
            web.env_file.as_array().unwrap()[0],
            "/project/.dip/env/base.env"
        );
    }
}
