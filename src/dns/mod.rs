pub mod server;

/// DNS resolver setup for local *.{tld} domains.
///
/// dip runs its own DNS server (server.rs) on a high port (default 5354).
/// macOS routes queries to it via /etc/resolver/<tld> files.
///
/// One-time init
/// -------------
/// `setup_once` takes ownership of /etc/resolver/ (sudo chown current user).
/// After that, dip can create/update/delete resolver files without sudo.
///
/// Ongoing sync
/// ------------
/// `sync_resolver_files` writes one /etc/resolver/<tld> per TLD in config
/// and removes stale files from previous configs.
use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::Result;

use crate::utils::output::Output;

const RESOLVER_DIR: &str = "/etc/resolver";

// ─── public API ───────────────────────────────────────────────────────────────

/// Called once at `dip proxy init`.
/// Creates /etc/resolver/ if missing, chowns it to the current user,
/// then writes resolver files for all configured TLDs.
pub fn setup_once(tlds: &[String], dns_port: u16, out: &Output) -> Result<()> {
    if cfg!(target_os = "macos") {
        setup_once_macos(tlds, dns_port, out)
    } else if cfg!(target_os = "linux") {
        setup_linux(tlds, dns_port, out)
    } else {
        out.warning("Automatic DNS setup is only supported on macOS and Linux");
        Ok(())
    }
}

/// Called whenever TLDs or DNS port change.
/// Writes new resolver files and removes stale ones (no sudo needed after init).
pub fn sync_resolver_files(tlds: &[String], dns_port: u16, out: &Output) {
    if !cfg!(target_os = "macos") {
        return;
    }

    // Write/update one file per TLD
    for tld in tlds {
        let path = format!("{RESOLVER_DIR}/{tld}");
        let content = resolver_content(dns_port);
        if resolver_needs_update(&path, &content) {
            match std::fs::write(&path, &content) {
                Ok(_) => out.success(&format!("/etc/resolver/{tld} → 127.0.0.1:{dns_port}")),
                Err(e) => out.warning(&format!("Failed to write /etc/resolver/{tld}: {e}")),
            }
        }
    }

    // Remove stale resolver files that belong to dip but are no longer in config
    remove_stale_resolvers(tlds, out);
}

// ─── macOS ────────────────────────────────────────────────────────────────────

fn setup_once_macos(tlds: &[String], dns_port: u16, out: &Output) -> Result<()> {
    // 1. Create /etc/resolver/ if it doesn't exist
    if !std::path::Path::new(RESOLVER_DIR).exists() {
        out.info("Creating /etc/resolver/ — requires sudo (one-time)...");
        let ok = Command::new("sudo")
            .args(["mkdir", "-p", RESOLVER_DIR])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            anyhow::bail!("Failed to create {RESOLVER_DIR}");
        }
    }

    // 2. Take ownership of /etc/resolver/ (dir + existing files) so future
    //    writes don't need sudo. We always run chown -R to catch files that
    //    were created by a previous root-owned dip run.
    if !we_own_resolver_dir() || has_root_owned_files() {
        out.info("Taking ownership of /etc/resolver/ — requires sudo (one-time)...");
        let user = current_user();
        let ok = Command::new("sudo")
            .args(["chown", "-R", &user, RESOLVER_DIR])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            out.success("You now own /etc/resolver/ — future DNS changes need no sudo");
        } else {
            out.warning(&format!(
                "Could not chown {RESOLVER_DIR} — run manually:\n  sudo chown -R {user} {RESOLVER_DIR}"
            ));
        }
    } else {
        out.info("/etc/resolver/ already owned by you");
    }

    // 3. Write resolver files for all configured TLDs
    sync_resolver_files(tlds, dns_port, out);

    Ok(())
}

// ─── Linux ────────────────────────────────────────────────────────────────────

fn setup_linux(tlds: &[String], dns_port: u16, out: &Output) -> Result<()> {
    let _ = dns_port; // systemd-resolved doesn't support custom ports natively
    if !std::path::Path::new("/run/systemd/resolve").exists() {
        out.warning(
            "systemd-resolved not found — configure DNS manually\n  \
             Add: nameserver 127.0.0.1 to /etc/resolv.conf",
        );
        return Ok(());
    }

    for tld in tlds {
        setup_systemd_resolved(tld, out);
    }
    Ok(())
}

fn setup_systemd_resolved(tld: &str, out: &Output) {
    let conf_dir = "/etc/systemd/resolved.conf.d";
    let conf_file = format!("{conf_dir}/dip-{tld}.conf");
    let content = format!("[Resolve]\nDNS=127.0.0.1\nDomains=~{tld}\n");

    let tmp = format!("/tmp/dip-resolved-{tld}.conf");
    if std::fs::write(&tmp, &content).is_ok() {
        let _ = Command::new("sudo")
            .args(["mkdir", "-p", conf_dir])
            .status();
        let ok = Command::new("sudo")
            .args(["mv", &tmp, &conf_file])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if ok {
            let _ = Command::new("sudo")
                .args(["systemctl", "restart", "systemd-resolved"])
                .status();
            out.success(&format!("systemd-resolved: .{tld} → 127.0.0.1"));
        } else {
            out.warning(&format!("Failed to configure systemd-resolved for .{tld}"));
        }
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn resolver_content(dns_port: u16) -> String {
    format!("nameserver 127.0.0.1\nport {dns_port}\n")
}

fn resolver_needs_update(path: &str, expected: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|c| c.trim() != expected.trim())
        .unwrap_or(true)
}

/// Returns true if the current process user owns /etc/resolver/.
fn we_own_resolver_dir() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(meta) = std::fs::metadata(RESOLVER_DIR) else {
            return false;
        };
        unsafe extern "C" {
            fn getuid() -> u32;
        }
        meta.uid() == unsafe { getuid() }
    }
    #[cfg(not(unix))]
    false
}

/// Returns true if any file inside /etc/resolver/ is owned by root (uid 0).
/// Used to detect when a previous dip run wrote files as root.
fn has_root_owned_files() -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(entries) = std::fs::read_dir(RESOLVER_DIR) else {
            return false;
        };
        entries
            .flatten()
            .any(|e| e.metadata().map(|m| m.uid() == 0).unwrap_or(false))
    }
    #[cfg(not(unix))]
    false
}

fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".to_string())
}

/// Remove /etc/resolver/<tld> files that dip created but are no longer configured.
/// Only removes files whose content matches the dip resolver format.
fn remove_stale_resolvers(active_tlds: &[String], out: &Output) {
    let Ok(entries) = std::fs::read_dir(RESOLVER_DIR) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        // Only touch files that look like they were written by dip
        // (contain "nameserver 127.0.0.1")
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        if !content.contains("nameserver 127.0.0.1") {
            continue;
        }

        // If this TLD is still active, skip it
        if active_tlds.iter().any(|t| t == name) {
            continue;
        }

        // Stale file — remove it
        if std::fs::remove_file(&path).is_ok() {
            out.info(&format!("Removed stale /etc/resolver/{name}"));
        }
    }
}

/// Write `content` to `path` using `sudo tee`.
#[allow(dead_code)]
fn sudo_write(path: &str, content: &str) -> bool {
    let mut child = match Command::new("sudo")
        .args(["tee", path])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(content.as_bytes());
    }

    child.wait().map(|s| s.success()).unwrap_or(false)
}
