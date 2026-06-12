//! Built-in SSH reverse tunnel for `dip share`.
//!
//! Spawns the system OpenSSH client (present out of the box on macOS/Linux)
//! to connect to localhost.run with a reverse port-forward. OpenSSH handles
//! the forwarding and proxying itself — we only watch its output for the
//! public URL assigned by the service.

use std::process::Stdio;
use std::sync::Arc;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::utils::output::Output;

const SSH_HOST: &str = "localhost.run";
const SSH_USER: &str = "nokey";

// ─── public entry point ───────────────────────────────────────────────────────

pub async fn run_tunnel(upstream: &str, out: &Output) -> Result<()> {
    let mut child = spawn_ssh(upstream)?;

    let stdout = child.stdout.take().expect("ssh stdout is piped");
    let stderr = child.stderr.take().expect("ssh stderr is piped");

    // Capture stderr for error reporting; surface lines that aren't known
    // ssh noise (e.g. the host-key warning produced by accept-new).
    let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_writer = Arc::clone(&captured);
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if !is_ssh_noise(&line) && !line.trim().is_empty() {
                eprintln!("  {line}");
            }
            captured_writer.lock().await.push(line);
        }
    });

    // Read stdout line by line, printing public URLs as they arrive,
    // while also waiting for Ctrl+C.
    let mut url_seen = false;
    let mut lines = BufReader::new(stdout).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if let Some(url) = extract_url(&line) {
                        out.success(&format!("  Public URL: {url}"));
                        url_seen = true;
                    } else if !line.trim().is_empty() {
                        println!("  {line}");
                    }
                }
                // EOF or read error — ssh exited or the connection dropped
                _ => break,
            },
            result = tokio::signal::ctrl_c() => {
                result.map_err(|e| anyhow::anyhow!("Signal error: {e}"))?;
                // The terminal delivers SIGINT to ssh too (same process group),
                // but kill explicitly so we never leave a child behind.
                let _ = child.kill().await;
                let _ = child.wait().await;
                stderr_task.abort();
                out.info("Disconnected");
                return Ok(());
            }
        }
    }

    // ssh exited on its own — reap it and finish collecting stderr.
    let status = child.wait().await?;
    let _ = stderr_task.await;

    if !url_seen {
        let stderr_output = captured.lock().await.join("\n");
        anyhow::bail!(
            "ssh exited before a public URL was received ({status}){}",
            if stderr_output.is_empty() {
                String::new()
            } else {
                format!("\n{stderr_output}")
            }
        );
    }

    out.info("Disconnected (tunnel closed by remote)");
    Ok(())
}

// ─── ssh process ──────────────────────────────────────────────────────────────

/// Spawn the system OpenSSH client with a reverse forward of remote port 80
/// to the local upstream (`host:port`).
fn spawn_ssh(upstream: &str) -> Result<Child> {
    Command::new("ssh")
        .arg("-T") // no PTY — we only read the service banner
        .args(["-o", "StrictHostKeyChecking=accept-new"])
        .args(["-o", "ServerAliveInterval=30"])
        .args(["-o", "ExitOnForwardFailure=yes"])
        .args(["-R", &format!("80:{upstream}")])
        .arg(format!("{SSH_USER}@{SSH_HOST}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!("ssh client not found — install OpenSSH client")
            } else {
                anyhow::anyhow!("Cannot start ssh: {e}")
            }
        })
}

/// Known noisy ssh messages that shouldn't pollute the tunnel output.
fn is_ssh_noise(line: &str) -> bool {
    // Printed by StrictHostKeyChecking=accept-new on first connect
    line.starts_with("Warning: Permanently added")
}

fn extract_url(line: &str) -> Option<&str> {
    line.split_whitespace()
        .find(|w| w.starts_with("https://") || w.starts_with("http://"))
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::extract_url;

    #[test]
    fn extracts_https_url_from_plain_line() {
        assert_eq!(
            extract_url("https://abc123.lhr.life"),
            Some("https://abc123.lhr.life")
        );
    }

    #[test]
    fn extracts_url_from_server_message_with_prefix_text() {
        // localhost.run typically prints something like:
        // "   == ======= ===== == https://abc.lhr.life"
        let line = "== ======= ===== == https://abc123def.lhr.life";
        assert_eq!(extract_url(line), Some("https://abc123def.lhr.life"));
    }

    #[test]
    fn extracts_http_url() {
        assert_eq!(
            extract_url("tunneled at http://abc.lhr.life"),
            Some("http://abc.lhr.life")
        );
    }

    #[test]
    fn prefers_first_url_when_multiple_present() {
        let line = "https://first.lhr.life or https://second.lhr.life";
        assert_eq!(extract_url(line), Some("https://first.lhr.life"));
    }

    #[test]
    fn returns_none_for_line_without_url() {
        assert_eq!(extract_url("connecting to localhost.run..."), None);
        assert_eq!(extract_url(""), None);
        assert_eq!(extract_url("   "), None);
    }

    #[test]
    fn does_not_match_bare_domain_without_scheme() {
        assert_eq!(extract_url("abc.lhr.life"), None);
    }

    #[test]
    fn handles_trailing_punctuation_in_url() {
        // URL at end of sentence — punctuation stays as part of the token
        // (we don't strip it, but the URL is still found)
        let line = "visit https://abc.lhr.life.";
        assert!(extract_url(line).is_some());
    }
}
