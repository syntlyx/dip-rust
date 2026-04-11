use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::dirs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    /// Port for plain HTTP — all requests are redirected to HTTPS
    #[serde(default = "default_http_port")]
    pub http_port: u16,
    /// Port for HTTPS
    #[serde(default = "default_https_port")]
    pub https_port: u16,
    /// Routing rules — exact matches always beat wildcards
    #[serde(default)]
    pub routes: Vec<Route>,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            http_port: 80,
            https_port: 443,
            routes: vec![],
        }
    }
}

fn default_http_port() -> u16 {
    80
}
fn default_https_port() -> u16 {
    443
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Route {
    /// Domain pattern — "api.foo.test" (exact) or "*.foo.test" (wildcard)
    pub domain: String,
    /// Upstream address — "host:port"
    pub upstream: String,
}

pub fn config_path() -> PathBuf {
    dirs::proxy_dir().join("config.toml")
}

pub fn load() -> Result<ProxyConfig> {
    let path = config_path();
    if !path.exists() {
        return Ok(ProxyConfig::default());
    }
    let content = std::fs::read_to_string(&path)?;
    toml::from_str(&content).map_err(|e| anyhow::anyhow!("Invalid proxy config: {e}"))
}

pub fn save(config: &ProxyConfig) -> Result<()> {
    let path = config_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let content = toml::to_string_pretty(config)?;
    std::fs::write(&path, content)?;
    Ok(())
}
