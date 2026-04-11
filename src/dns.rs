/// DNS setup for local *.{tld} domains without Pi-hole.
///
/// macOS:  dnsmasq (via Homebrew) on port 5353 + /etc/resolver/<tld>
///         Port 5353 means dnsmasq runs without sudo.
///         Only /etc/resolver/<tld> requires a one-time sudo.
///
/// Linux:  dnsmasq via system package manager on port 53.
///         systemd-resolved configured to forward .<tld> to 127.0.0.1.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::Result;

use crate::utils::output::Output;

// 5353 is taken by mDNSResponder (Bonjour) on macOS
const DNSMASQ_PORT_MACOS: u16 = 5380;

pub fn setup(tld: &str, out: &Output) -> Result<()> {
    if cfg!(target_os = "macos") {
        setup_macos(tld, out)
    } else if cfg!(target_os = "linux") {
        setup_linux(tld, out)
    } else {
        out.warning("Automatic DNS setup is only supported on macOS and Linux");
        Ok(())
    }
}

// ─── macOS ────────────────────────────────────────────────────────────────────

fn setup_macos(tld: &str, out: &Output) -> Result<()> {
    // 1. Ensure Homebrew is available
    if !cmd_exists("brew") {
        out.warning(
            "Homebrew not found — install it from https://brew.sh, then re-run `dip proxy init`",
        );
        return Ok(());
    }

    // 2. Install dnsmasq if needed
    if cmd_exists("dnsmasq") {
        out.info("dnsmasq already installed");
    } else {
        out.info("Installing dnsmasq via Homebrew...");
        let ok = Command::new("brew")
            .args(["install", "dnsmasq"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            anyhow::bail!("brew install dnsmasq failed");
        }
    }

    // 3. Configure dnsmasq — port 5353 (no sudo) + wildcard address
    let prefix = brew_prefix()?;
    let conf_path = format!("{prefix}/etc/dnsmasq.conf");

    ensure_line(&conf_path, &format!("port={DNSMASQ_PORT_MACOS}"), false)?;
    ensure_line(&conf_path, &format!("address=/.{tld}/127.0.0.1"), false)?;
    out.success(&format!(
        "dnsmasq configured: port={DNSMASQ_PORT_MACOS}, address=/.{tld}/127.0.0.1"
    ));

    // 4. Start / restart dnsmasq as a user service (no sudo needed for port 5353)
    out.info("Starting dnsmasq service...");
    let ok = Command::new("brew")
        .args(["services", "restart", "dnsmasq"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok {
        out.success("dnsmasq service running");
    } else {
        out.warning("Could not start dnsmasq — run: brew services restart dnsmasq");
    }

    // 5. Create /etc/resolver/<tld> (one-time sudo)
    let resolver_file = format!("/etc/resolver/{tld}");
    let expected = format!("nameserver 127.0.0.1\nport {DNSMASQ_PORT_MACOS}\n");

    if resolver_needs_update(&resolver_file, &expected) {
        out.info(&format!(
            "Creating /etc/resolver/{tld} — requires sudo (one-time)..."
        ));
        let ok = Command::new("sudo")
            .args(["mkdir", "-p", "/etc/resolver"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            let ok = sudo_write(&resolver_file, &expected);
            if ok {
                out.success(format!("/etc/resolver/{tld} created").as_str());
            } else {
                out.warning(&format!(
                    "Failed to write /etc/resolver/{tld} — run manually:\n  \
                     sudo mkdir -p /etc/resolver\n  \
                     printf 'nameserver 127.0.0.1\\nport {DNSMASQ_PORT_MACOS}\\n' | sudo tee /etc/resolver/{tld}"
                ));
            }
        }
    } else {
        out.info(&format!("/etc/resolver/{tld} already configured"));
    }

    Ok(())
}

// ─── Linux ────────────────────────────────────────────────────────────────────

fn setup_linux(tld: &str, out: &Output) -> Result<()> {
    // 1. Install dnsmasq if needed
    if cmd_exists("dnsmasq") {
        out.info("dnsmasq already installed");
    } else {
        out.info("Installing dnsmasq...");
        if !install_dnsmasq_linux(out) {
            out.warning(
                "Could not install dnsmasq automatically — \
                 install it manually and re-run `dip proxy init`",
            );
            return Ok(());
        }
    }

    // 2. Configure dnsmasq
    let conf_path = "/etc/dnsmasq.conf";
    let line = format!("address=/.{tld}/127.0.0.1");

    let content = std::fs::read_to_string(conf_path).unwrap_or_default();
    if !content.lines().any(|l| l.trim() == line) {
        out.info("Configuring dnsmasq (requires sudo)...");
        let ok = Command::new("sudo")
            .args(["sh", "-c", &format!("echo '{line}' >> {conf_path}")])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            out.warning(&format!(
                "Failed to configure dnsmasq — add manually to {conf_path}:\n  {line}"
            ));
        }
    }
    out.success(&format!("dnsmasq: address=/.{tld}/127.0.0.1"));

    // 3. Restart dnsmasq
    let _ = Command::new("sudo")
        .args(["systemctl", "restart", "dnsmasq"])
        .stderr(Stdio::null())
        .status();

    // 4. Configure systemd-resolved if present
    if Path::new("/run/systemd/resolve").exists() {
        setup_systemd_resolved(tld, out);
    }

    Ok(())
}

fn install_dnsmasq_linux(out: &Output) -> bool {
    let managers: &[(&str, &[&str])] = &[
        ("apt-get", &["sudo", "apt-get", "install", "-y", "dnsmasq"]),
        ("dnf", &["sudo", "dnf", "install", "-y", "dnsmasq"]),
        (
            "pacman",
            &["sudo", "pacman", "-Sy", "--noconfirm", "dnsmasq"],
        ),
        ("zypper", &["sudo", "zypper", "install", "-y", "dnsmasq"]),
    ];

    for (pm, cmd) in managers {
        if cmd_exists(pm) {
            out.info(&format!("Using {pm}..."));
            return Command::new(cmd[0])
                .args(&cmd[1..])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
        }
    }
    false
}

fn setup_systemd_resolved(tld: &str, out: &Output) {
    let conf_dir = "/etc/systemd/resolved.conf.d";
    let conf_file = format!("{conf_dir}/dip-{tld}.conf");
    let content = format!("[Resolve]\nDNS=127.0.0.1\nDomains=~{tld}\n");

    let _ = Command::new("sudo")
        .args(["mkdir", "-p", conf_dir])
        .status();

    let tmp = format!("/tmp/dip-resolved-{tld}.conf");
    if std::fs::write(&tmp, &content).is_ok() {
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
            out.warning(
                "Failed to configure systemd-resolved — DNS may not resolve outside dnsmasq",
            );
        }
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn brew_prefix() -> Result<String> {
    let out = Command::new("brew").arg("--prefix").output()?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Append `line` to `path` if it isn't already present.
/// `allow_sudo` — retry with sudo if write fails (used for system files).
fn ensure_line(path: &str, line: &str, allow_sudo: bool) -> Result<()> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    if content.lines().any(|l| l.trim() == line) {
        return Ok(());
    }
    let append = format!("{line}\n");
    match std::fs::OpenOptions::new().append(true).open(path) {
        Ok(mut f) => f.write_all(append.as_bytes())?,
        Err(_) if allow_sudo => {
            Command::new("sudo")
                .args(["sh", "-c", &format!("echo '{line}' >> {path}")])
                .status()?;
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

fn resolver_needs_update(path: &str, expected: &str) -> bool {
    std::fs::read_to_string(path)
        .map(|c| c.trim() != expected.trim())
        .unwrap_or(true)
}

/// Write `content` to `path` using `sudo tee`.
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

    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        let _ = stdin.write_all(content.as_bytes());
    }

    child.wait().map(|s| s.success()).unwrap_or(false)
}

fn cmd_exists(cmd: &str) -> bool {
    Command::new("which")
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
