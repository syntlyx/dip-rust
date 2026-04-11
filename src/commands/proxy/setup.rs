//! Proxy setup — `dip proxy init` and `dip proxy config`.

use anyhow::Result;
use colored::Colorize;

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

    // 5. Optionally configure DNS
    println!();
    if prompt_yes(
        "Set up DNS for *.test automatically? (skip if you use Pi-hole or handle DNS yourself) [y/N]",
    ) {
        out.info("Setting up DNS...");
        if let Err(e) = dns::setup_once(&existing_cfg.tlds, existing_cfg.dns_port, &out) {
            out.warning(&format!("DNS setup failed: {e}"));
        }
    } else {
        out.info("Skipping DNS setup — make sure *.test resolves to 127.0.0.1");
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
