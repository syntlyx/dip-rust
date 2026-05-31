use anyhow::{Context, Result};

use crate::runtime::Runtime;
pub use crate::runtime::compose_file::{BuildConfig, ComposeConfig, dockerfile_stages};

pub fn load(rt: &Runtime) -> Result<ComposeConfig> {
    let raw = rt
        .compose_capture(&["config", "--format", "json"])
        .context("failed to resolve Compose config")?;
    serde_json::from_str(&raw).context("failed to parse Compose JSON config")
}
