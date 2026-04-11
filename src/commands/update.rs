use anyhow::Result;

use crate::config;
use crate::utils::output::Output;

pub fn run(no_color: bool) -> Result<()> {
    let out = Output::new(no_color);

    // Detect current platform → release asset name
    let target = detect_target()?;
    let current_version = env!("CARGO_PKG_VERSION");

    out.info(&format!("Current version: v{current_version}"));
    out.info("Checking latest release...");

    // GitHub API: latest release
    let api_url = format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        config::REPO_OWNER,
        config::REPO_NAME,
    );

    let api_resp = curl_get(&api_url)?;
    let json: serde_json::Value = serde_json::from_str(&api_resp)
        .map_err(|e| anyhow::anyhow!("Failed to parse GitHub API response: {e}"))?;

    let latest_version = json["tag_name"]
        .as_str()
        .ok_or_else(|| {
            anyhow::anyhow!("No tag_name in GitHub response — is this a public repo with releases?")
        })?
        .trim_start_matches('v');

    if latest_version == current_version {
        out.success(&format!("Already up to date (v{current_version})"));
        return Ok(());
    }

    out.info(&format!("New version available: v{latest_version}"));

    // Find the asset URL for our platform
    let asset_name = format!("dip-{target}");
    let asset_url = json["assets"]
        .as_array()
        .and_then(|assets| {
            assets
                .iter()
                .find(|a| a["name"].as_str() == Some(&asset_name))
                .and_then(|a| a["browser_download_url"].as_str())
                .map(str::to_string)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No release asset '{asset_name}' found for this platform.\n\
                 Available targets: check https://github.com/{}/{}/releases",
                config::REPO_OWNER,
                config::REPO_NAME,
            )
        })?;

    out.info(&format!("Downloading {asset_name}..."));

    // Download to a temp file next to the current binary
    let current_exe = std::env::current_exe()
        .map_err(|e| anyhow::anyhow!("Cannot determine current executable path: {e}"))?;
    let tmp_path = current_exe.with_extension("tmp");

    curl_download(&asset_url, &tmp_path)?;

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755))?;
    }

    // Atomically replace the current binary
    // On Unix this works even while the process is running (inode swap)
    std::fs::rename(&tmp_path, &current_exe).map_err(|e| {
        let _ = std::fs::remove_file(&tmp_path);
        anyhow::anyhow!(
            "Failed to replace binary at {}: {e}\n\
             Try running with sudo if dip is installed in a system directory.",
            current_exe.display()
        )
    })?;

    out.success(&format!(
        "Updated to v{latest_version} → {}",
        current_exe.display()
    ));

    Ok(())
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Map current OS + arch to a release asset suffix, e.g. "aarch64-apple-darwin".
fn detect_target() -> Result<String> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    let target = match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        _ => anyhow::bail!("Unsupported platform: {os}/{arch}"),
    };

    Ok(target.to_string())
}

fn curl_get(url: &str) -> Result<String> {
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "--user-agent",
            concat!("dip/", env!("CARGO_PKG_VERSION")),
            url,
        ])
        .output()
        .map_err(|e| anyhow::anyhow!("curl not found: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("curl failed: {}", stderr.trim());
    }

    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn curl_download(url: &str, dest: &std::path::Path) -> Result<()> {
    let dest_str = dest
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Download path is not valid UTF-8: {}", dest.display()))?;

    let status = std::process::Command::new("curl")
        .args([
            "-fSL",
            "--progress-bar",
            "--user-agent",
            concat!("dip/", env!("CARGO_PKG_VERSION")),
            "-o",
            dest_str,
            url,
        ])
        .status()
        .map_err(|e| anyhow::anyhow!("curl not found: {e}"))?;

    if !status.success() {
        anyhow::bail!("Download failed (exit {status})");
    }
    Ok(())
}
