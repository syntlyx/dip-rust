//! Env-var parsing helpers shared across the codebase.
//!
//! Three call sites previously duplicated this logic:
//! - `project.rs`  — reading `.dip/.env` / `default.env` files
//! - `hooks.rs`    — parsing hook stdout (supports `export KEY=VALUE`, quotes)
//! - `db/mod.rs`   — parsing Docker inspect JSON env array `["KEY=VALUE", …]`

use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

// ─── core ─────────────────────────────────────────────────────────────────────

/// Parse a single `KEY=VALUE` line into `(key, value)`.
///
/// Handles:
/// - Leading `export ` keyword (stripped)
/// - Surrounding `"…"` / `'…'` quotes on the value (stripped)
/// - `#` comments → `None`
/// - Blank lines → `None`
/// - Values that contain `=` (only the first `=` is the separator)
pub fn parse_env_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    // Strip leading `export ` keyword
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (key, value) = line.split_once('=')?;
    let key = key.trim().to_string();
    let value = value
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .to_string();
    Some((key, value))
}

// ─── file parsing ─────────────────────────────────────────────────────────────

/// Read and parse a `KEY=VALUE` env file into a `HashMap`.
///
/// Blank lines and `#` comments are ignored.
/// Values may optionally be quoted.
pub fn parse_env_file(path: &std::path::Path) -> Result<HashMap<String, String>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {e}", path.display()))?;
    Ok(parse_env_str(&content))
}

// ─── string parsing ───────────────────────────────────────────────────────────

/// Parse a multi-line `KEY=VALUE` string (e.g. hook stdout) into a `HashMap`.
///
/// Supports `export KEY=VALUE`, quoted values, comments, blank lines.
pub fn parse_env_str(s: &str) -> HashMap<String, String> {
    s.lines().filter_map(parse_env_line).collect()
}

// ─── Docker JSON array parsing ────────────────────────────────────────────────

/// Parse Docker inspect's env array — `["KEY=VALUE", …]` — into a `HashMap`.
///
/// Malformed or non-string entries are silently skipped.
pub fn parse_env_json_array(env_array: &Value) -> HashMap<String, String> {
    let Some(arr) = env_array.as_array() else {
        return HashMap::new();
    };
    arr.iter()
        .filter_map(|entry| parse_env_line(entry.as_str()?))
        .collect()
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_key_value() {
        let out = parse_env_str("FOO=bar\nBAZ=qux\n");
        assert_eq!(out["FOO"], "bar");
        assert_eq!(out["BAZ"], "qux");
    }

    #[test]
    fn export_prefix_stripped() {
        let out = parse_env_str("export AWS_ACCESS_KEY_ID=AKIA123\nexport AWS_SECRET=secret\n");
        assert_eq!(out["AWS_ACCESS_KEY_ID"], "AKIA123");
        assert_eq!(out["AWS_SECRET"], "secret");
    }

    #[test]
    fn quoted_values_stripped() {
        let out = parse_env_str("export TOKEN=\"abc123\"\nKEY='xyz'\n");
        assert_eq!(out["TOKEN"], "abc123");
        assert_eq!(out["KEY"], "xyz");
    }

    #[test]
    fn comments_and_blank_lines_ignored() {
        let out = parse_env_str("# comment\n\nFOO=bar\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out["FOO"], "bar");
    }

    #[test]
    fn value_with_equals_sign() {
        // Only the first '=' is the separator
        let out = parse_env_str("JDBC=jdbc:mysql://host/db?user=root\n");
        assert_eq!(out["JDBC"], "jdbc:mysql://host/db?user=root");
    }

    #[test]
    fn json_array_parsed() {
        let arr = serde_json::json!(["FOO=bar", "BAZ=qux", "invalid"]);
        let out = parse_env_json_array(&arr);
        assert_eq!(out["FOO"], "bar");
        assert_eq!(out["BAZ"], "qux");
        assert!(!out.contains_key("invalid"));
    }

    #[test]
    fn json_array_null_returns_empty() {
        let out = parse_env_json_array(&Value::Null);
        assert!(out.is_empty());
    }
}
