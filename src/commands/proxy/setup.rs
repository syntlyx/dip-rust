//! Proxy setup — `dip proxy init` and `dip proxy config`.

use crate::utils::style::Stylize;
use anyhow::Result;

use crate::dns;
use crate::proxy::certs;
use crate::proxy::config::{self, ProxyConfig};
use crate::utils::output::Output;

use super::daemon::{daemon_pid, run_stop, spawn_daemon};

// ─── public commands ──────────────────────────────────────────────────────────

/// `dip proxy init` — generate CA + server cert, install CA in system keychain.
pub fn run_init(no_color: bool) -> Result<()> {
    let out = Output::new(no_color);

    std::fs::create_dir_all(crate::dirs::proxy_dir())?;

    // 1. Ensure CA exists
    out.info("Setting up Certificate Authority...");
    certs::ensure_ca()?;
    out.success(&format!("CA: {}", certs::ca_cert_path().display()));

    // 2. Install CA in system keychain
    out.info("Installing CA in system keychain (may prompt for password)...");
    match certs::install_ca() {
        Ok(true) => out.success("CA installed — all dip HTTPS domains are now trusted"),
        Ok(false) => out.info("CA already trusted"),
        Err(e) => out.warning(&format!(
            "CA install failed: {e}\nInstall manually from: {}",
            certs::ca_cert_path().display()
        )),
    }

    // 3. Generate / refresh server cert with current routes as SANs
    out.info("Generating server certificate...");
    let existing_cfg = config::load().unwrap_or_default();
    let domains: Vec<String> = existing_cfg
        .routes
        .iter()
        .map(|r| r.domain.clone())
        .collect();
    let cert_changed = certs::ensure_server_cert(&domains)?;
    if cert_changed {
        out.success(&format!(
            "Cert regenerated: {}",
            certs::srv_cert_path().display()
        ));
    } else {
        out.success(&format!("Cert: {}", certs::srv_cert_path().display()));
    }

    // 4. Write default config if missing
    if !config::config_path().exists() {
        config::save(&ProxyConfig::default())?;
        out.success(&format!("Config: {}", config::config_path().display()));
    }

    // 5. Interactive DNS wizard
    println!();
    let tld = prompt_input("TLD for local domains", "test");
    if prompt_yes(&format!(
        "Set up DNS for *.{tld} automatically? (skip if you use Pi-hole or handle DNS yourself) [y/N]"
    )) {
        let system_dns = read_system_dns();
        let default_port: u16 = if cfg!(target_os = "linux") { 53 } else { 5354 };

        let port_str = prompt_input("DNS port", &default_port.to_string());
        let port: u16 = port_str.trim().parse().unwrap_or(default_port);
        let dns_default = system_dns.join(" ");
        let dns_input = prompt_input("Upstream DNS servers (space-separated)", &dns_default);
        let upstream_dns: Vec<String> = dns_input
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();

        let mut cfg = config::load().unwrap_or_default();
        cfg.tlds = vec![tld];
        cfg.dns_port = port;
        if !upstream_dns.is_empty() {
            cfg.upstream_dns = upstream_dns;
        }
        config::save(&cfg)?;

        // On Linux: if port < 1024, grant cap_net_bind_service so dip can bind it without root
        #[cfg(target_os = "linux")]
        if port < 1024 {
            setup_net_bind_cap(&out);
        }

        out.info("Setting up DNS...");
        if let Err(e) = dns::setup_once(&cfg.tlds, cfg.dns_port, &out) {
            out.warning(&format!("DNS setup failed: {e}"));
        }
    } else {
        // Still save the chosen TLD even if DNS setup is skipped
        let mut cfg = config::load().unwrap_or_default();
        cfg.tlds = vec![tld.clone()];
        config::save(&cfg)?;
        out.info(&format!(
            "Skipping DNS setup — make sure *.{tld} resolves to 127.0.0.1"
        ));
    }

    println!();
    out.info("Next: run `dip start` inside a project — routes will be set up automatically.");
    Ok(())
}

/// `dip proxy config` — show or update DNS / port settings.
pub fn run_config(tld: Option<&str>, dns_port: Option<u16>, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let mut cfg = config::load()?;
    let mut changed = false;

    if let Some(tld_arg) = tld {
        let new_tlds: Vec<String> = tld_arg
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if new_tlds != cfg.tlds {
            cfg.tlds = new_tlds;
            changed = true;
        }
    }

    if let Some(port) = dns_port
        && port != cfg.dns_port
    {
        cfg.dns_port = port;
        changed = true;
    }

    if changed {
        config::save(&cfg)?;
        out.success("Config saved");

        dns::sync_resolver_files(&cfg.tlds, cfg.dns_port, &out);

        if daemon_pid().is_some() {
            out.info("Restarting proxy to apply changes...");
            run_stop(no_color)?;
            spawn_daemon(&out, false)?;
        }
    }

    out.section("proxy config", || {
        println!("  {:18} :{}", "HTTP:".dimmed(), cfg.http_port);
        println!("  {:18} :{}", "HTTPS:".dimmed(), cfg.https_port);
        println!(
            "  {:18} :{} (built-in DNS server)",
            "DNS port:".dimmed(),
            cfg.dns_port
        );
        println!(
            "  {:18} {}",
            "TLDs:".dimmed(),
            cfg.tlds
                .iter()
                .map(|t| format!("*.{t}"))
                .collect::<Vec<_>>()
                .join(", ")
                .cyan()
        );
        println!(
            "  {:18} {}",
            "Upstream DNS:".dimmed(),
            cfg.upstream_dns.join(", ").cyan()
        );
    });

    Ok(())
}

// ─── helpers ──────────────────────────────────────────────────────────────────

fn prompt_yes(question: &str) -> bool {
    use std::io::{self, BufRead, Write};
    print!("  {question} ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).ok();
    matches!(line.trim().to_lowercase().as_str(), "y" | "yes")
}

fn prompt_input(question: &str, default: &str) -> String {
    use std::io::{self, BufRead, Write};
    print!("  {question} [{default}]: ");
    let _ = io::stdout().flush();
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line).ok();
    let trimmed = line.trim().to_string();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed
    }
}

/// Read real upstream DNS servers from the system.
/// On Linux with systemd-resolved, reads from /run/systemd/resolve/resolv.conf
/// which contains actual upstreams (not the 127.0.0.53 stub).
/// Falls back to parsing /etc/resolv.conf and filtering out loopback addresses.
fn read_system_dns() -> Vec<String> {
    let candidates = [
        "/run/systemd/resolve/resolv.conf", // systemd-resolved: real upstreams
        "/etc/resolv.conf",
    ];

    for path in &candidates {
        if let Ok(content) = std::fs::read_to_string(path) {
            let servers: Vec<String> = content
                .lines()
                .filter_map(|line| {
                    let line = line.trim();
                    let addr = line.strip_prefix("nameserver")?.trim();
                    // Filter out loopback — those are stubs, not real upstreams
                    if addr.starts_with("127.") || addr == "::1" {
                        return None;
                    }
                    Some(addr.to_string())
                })
                .take(2)
                .collect();

            if !servers.is_empty() {
                return servers;
            }
        }
    }

    vec!["1.1.1.1".to_string(), "8.8.8.8".to_string()]
}

/// Grant cap_net_bind_service to the current dip binary so it can bind
/// privileged ports (< 1024, e.g. port 53) without running as root.
/// Note: this capability is cleared when the binary is replaced (e.g. after
/// `cargo install` update) — re-run `dip proxy init` to restore it.
#[cfg(target_os = "linux")]
fn setup_net_bind_cap(out: &Output) {
    let Ok(exe) = std::env::current_exe() else {
        out.warning("Could not determine dip binary path — skip setcap");
        return;
    };
    let exe_str = exe.to_string_lossy();
    out.info(&format!(
        "Granting cap_net_bind_service to {exe_str} (one-time sudo)..."
    ));
    let ok = std::process::Command::new("sudo")
        .args(["setcap", "cap_net_bind_service+ep", &*exe_str])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        out.success("dip can now bind port 53 without root");
    } else {
        out.warning(&format!(
            "Could not set capability — run manually:\n  \
             sudo setcap cap_net_bind_service+ep {exe_str}\n  \
             Or use DNS port 5354 and re-run `dip proxy init`\n  \
             Note: re-run `dip proxy init` after updating dip to restore this."
        ));
    }
}
