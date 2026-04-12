//! Built-in SSH reverse tunnel for `dip share`.
//!
//! Connects to localhost.run via pure-Rust SSH (russh) — no cloudflared,
//! no system ssh binary. Registers a reverse port-forward and proxies each
//! incoming internet connection to a local TCP port.

use std::sync::Arc;

use anyhow::Result;
use russh::client::{self, Msg};
use russh::keys::PublicKey;
use russh::{Channel, ChannelMsg};
use tokio::net::TcpStream;

use crate::utils::output::Output;

const SSH_HOST: &str = "ssh.localhost.run";
const SSH_PORT: u16 = 22;
const SSH_USER: &str = "nokey";

// ─── public entry point ───────────────────────────────────────────────────────

pub async fn run_tunnel(upstream: &str, out: &Output) -> Result<()> {
    let config = Arc::new(client::Config::default());
    let handler = TunnelHandler {
        upstream: upstream.to_string(),
    };

    let mut session = client::connect(config, (SSH_HOST, SSH_PORT), handler)
        .await
        .map_err(|e| anyhow::anyhow!("Cannot connect to {SSH_HOST}: {e}"))?;

    let auth = session
        .authenticate_none(SSH_USER)
        .await
        .map_err(|e| anyhow::anyhow!("SSH auth failed: {e}"))?;

    if !auth.success() {
        anyhow::bail!("localhost.run rejected the connection (none-auth not accepted)");
    }

    // Ask the server to forward remote port 80 → us → local_port.
    // Must be called before opening a session channel so the server is ready
    // to accept forwarded connections when it sends us the public URL.
    session.tcpip_forward("localhost", 80).await?;

    // Open a session channel and request a shell — localhost.run only sends
    // the assigned public URL after the session is active (shell running).
    let mut ch = session.channel_open_session().await?;
    ch.request_shell(false).await?;

    // Read the session channel concurrently with waiting for Ctrl+C.
    tokio::select! {
        _ = drain_session_channel(&mut ch, out) => {
            // Session channel closed by server (unusual but possible)
        }
        result = tokio::signal::ctrl_c() => {
            result.map_err(|e| anyhow::anyhow!("Signal error: {e}"))?;
        }
    }

    out.info("Disconnected");
    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;

    Ok(())
}

// ─── session channel reader ───────────────────────────────────────────────────

/// Read the SSH session channel until it closes, printing public URLs as they arrive.
async fn drain_session_channel(ch: &mut Channel<Msg>, out: &Output) {
    loop {
        match ch.wait().await {
            Some(ChannelMsg::Data { data }) => {
                let text = String::from_utf8_lossy(&data);
                for line in text.lines() {
                    if let Some(url) = extract_url(line) {
                        out.success(&format!("  Public URL: {url}"));
                    } else if !line.trim().is_empty() {
                        println!("  {line}");
                    }
                }
            }
            Some(ChannelMsg::ExtendedData { data, .. }) => {
                // Stderr from the server (some messages come here)
                let text = String::from_utf8_lossy(&data);
                for line in text.lines() {
                    if let Some(url) = extract_url(line) {
                        out.success(&format!("  Public URL: {url}"));
                    } else if !line.trim().is_empty() {
                        println!("  {line}");
                    }
                }
            }
            None => break,
            _ => {}
        }
    }
}

fn extract_url(line: &str) -> Option<&str> {
    line.split_whitespace()
        .find(|w| w.starts_with("https://") || w.starts_with("http://"))
}

// ─── russh client handler ────────────────────────────────────────────────────

struct TunnelHandler {
    upstream: String,
}

impl client::Handler for TunnelHandler {
    type Error = anyhow::Error;

    // Accept any server key — we're connecting to a public tunnel service,
    // not a machine where TOFU/pinning matters for security.
    async fn check_server_key(
        &mut self,
        _server_public_key: &PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }

    /// Called when the SSH server opens a forwarded-tcpip channel, i.e. an
    /// internet request arrived at localhost.run and is being forwarded to us.
    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        _connected_address: &str,
        _connected_port: u32,
        _originator_address: &str,
        _originator_port: u32,
        _session: &mut client::Session,
    ) -> Result<(), Self::Error> {
        let upstream = self.upstream.clone();
        tokio::spawn(async move {
            if let Err(e) = proxy_channel(channel, &upstream).await {
                eprintln!("  [tunnel] proxy error: {e}");
            }
        });
        Ok(())
    }
}

// ─── per-connection proxy ────────────────────────────────────────────────────

/// Bidirectionally copy between an SSH forwarded-tcpip channel and the upstream TCP socket.
async fn proxy_channel(channel: Channel<Msg>, upstream: &str) -> Result<()> {
    let mut stream = TcpStream::connect(upstream)
        .await
        .map_err(|e| anyhow::anyhow!("Cannot connect to {upstream}: {e}"))?;

    let mut channel_stream = channel.into_stream();
    tokio::io::copy_bidirectional(&mut channel_stream, &mut stream)
        .await
        .map_err(|e| anyhow::anyhow!("Proxy IO error: {e}"))?;

    Ok(())
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
