use colored::Colorize;

use crate::proxy::{certs, config};
use crate::utils::output::Output;

// ─── check result ─────────────────────────────────────────────────────────────

enum Status {
    Ok(String),
    Warn(String, &'static str), // (message, fix hint)
    Fail(String, &'static str),
}

struct Check {
    label: &'static str,
    status: Status,
}

impl Check {
    fn ok(label: &'static str, msg: impl Into<String>) -> Self {
        Self {
            label,
            status: Status::Ok(msg.into()),
        }
    }
    fn warn(label: &'static str, msg: impl Into<String>, fix: &'static str) -> Self {
        Self {
            label,
            status: Status::Warn(msg.into(), fix),
        }
    }
    fn fail(label: &'static str, msg: impl Into<String>, fix: &'static str) -> Self {
        Self {
            label,
            status: Status::Fail(msg.into(), fix),
        }
    }
}

// ─── public entry point ───────────────────────────────────────────────────────

pub fn run(no_color: bool) {
    let out = Output::new(no_color);
    let checks = collect_checks();

    let failures = checks
        .iter()
        .filter(|c| matches!(c.status, Status::Fail(..)))
        .count();
    let warnings = checks
        .iter()
        .filter(|c| matches!(c.status, Status::Warn(..)))
        .count();

    out.section("doctor", || {
        let label_width = checks.iter().map(|c| c.label.len()).max().unwrap_or(0) + 2;

        for check in &checks {
            let (icon, msg, fix) = match &check.status {
                Status::Ok(m) => ("✓".green(), m.as_str().normal(), None),
                Status::Warn(m, f) => ("⚠".yellow(), m.as_str().normal(), Some(*f)),
                Status::Fail(m, f) => ("✗".red(), m.as_str().normal(), Some(*f)),
            };

            println!(
                "  {icon} {:<width$} {}",
                format!("{}:", check.label).dimmed(),
                msg,
                width = label_width,
            );

            if let Some(hint) = fix {
                println!("    {} {}", "→".dimmed(), hint.dimmed());
            }
        }

        println!();
        if failures > 0 {
            println!(
                "  {} {} failed, {} warning(s)",
                "✗".red().bold(),
                failures,
                warnings,
            );
        } else if warnings > 0 {
            println!(
                "  {} {} warning(s) — everything else looks good",
                "⚠".yellow(),
                warnings
            );
        } else {
            println!("  {} All checks passed", "✓".green().bold());
        }
    });
}

// ─── individual checks ────────────────────────────────────────────────────────

fn collect_checks() -> Vec<Check> {
    let cfg = config::load().unwrap_or_default();

    vec![
        check_container_runtime(),
        check_proxy(),
        check_ca_cert(),
        check_server_cert(),
        check_ca_trusted(),
        check_dns(&cfg),
        check_ports(&cfg),
        #[cfg(target_os = "linux")]
        check_linux_caps(&cfg),
    ]
}

fn check_container_runtime() -> Check {
    let runtime = crate::runtime::Runtime::active_name();
    match crate::runtime::Runtime::check_daemon() {
        Ok(()) if runtime == "apple" => Check::ok("Apple Container", "service is running"),
        Ok(()) => Check::ok("Docker", "daemon is running"),
        Err(_) if runtime == "apple" => Check::fail(
            "Apple Container",
            "service is not running",
            "install Apple Container and run: container system start",
        ),
        Err(_) => Check::fail(
            "Docker",
            "daemon is not running",
            "start Docker Desktop, OrbStack, Colima, or `sudo systemctl start docker`",
        ),
    }
}

fn check_proxy() -> Check {
    use crate::commands::proxy::daemon_pid;

    match daemon_pid() {
        Some(pid) => Check::ok("Proxy", format!("running (PID {pid})")),
        None => Check::warn("Proxy", "not running", "run: dip proxy start"),
    }
}

fn check_ca_cert() -> Check {
    let path = certs::ca_cert_path();
    if !path.exists() {
        return Check::fail("CA cert", "not found", "run: dip proxy init");
    }
    match cert_days_left(&path) {
        Some(days) if days < 0 => Check::fail(
            "CA cert",
            format!("EXPIRED {} days ago", -days),
            "run: dip proxy init",
        ),
        Some(days) if days < 30 => Check::warn(
            "CA cert",
            format!("expires in {days} days"),
            "run: dip proxy init",
        ),
        Some(days) => Check::ok("CA cert", format!("valid, expires in {days} days")),
        None => Check::warn(
            "CA cert",
            "could not parse certificate",
            "run: dip proxy init",
        ),
    }
}

fn check_server_cert() -> Check {
    let path = certs::srv_cert_path();
    if !path.exists() {
        return Check::warn(
            "Server cert",
            "not generated yet",
            "run: dip start inside a project",
        );
    }
    match cert_days_left(&path) {
        Some(days) if days < 0 => Check::fail(
            "Server cert",
            format!("EXPIRED {} days ago", -days),
            "run: dip proxy init",
        ),
        Some(days) if days < 30 => Check::warn(
            "Server cert",
            format!("expires in {days} days"),
            "run: dip proxy init",
        ),
        Some(days) => Check::ok("Server cert", format!("valid, expires in {days} days")),
        None => Check::warn(
            "Server cert",
            "could not parse certificate",
            "run: dip proxy init",
        ),
    }
}

fn check_ca_trusted() -> Check {
    #[cfg(target_os = "macos")]
    {
        let trusted = std::process::Command::new("security")
            .args([
                "find-certificate",
                "-c",
                "dip Local CA",
                "/Library/Keychains/System.keychain",
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if trusted {
            return Check::ok("CA trusted", "installed in system keychain");
        } else {
            return Check::fail(
                "CA trusted",
                "not in system keychain",
                "run: dip proxy init",
            );
        }
    }

    #[cfg(target_os = "linux")]
    {
        let ca_path = certs::ca_cert_path();
        let store_paths = [
            "/etc/ssl/certs/dip-ca.pem",
            "/usr/local/share/ca-certificates/dip-ca.crt",
            "/etc/pki/ca-trust/source/anchors/dip-ca.crt",
            "/etc/ca-certificates/trust-source/anchors/dip-ca.crt",
        ];
        let installed = store_paths.iter().any(|p| std::path::Path::new(p).exists());
        if installed || !ca_path.exists() {
            return Check::ok("CA trusted", "found in system cert store");
        } else {
            return Check::fail(
                "CA trusted",
                "not in system cert store",
                "run: dip proxy init",
            );
        }
    }

    #[allow(unreachable_code)]
    Check::warn(
        "CA trusted",
        "cannot verify on this platform",
        "install CA manually",
    )
}

fn check_dns(cfg: &config::ProxyConfig) -> Check {
    let tld = cfg.tlds.first().map(String::as_str).unwrap_or("test");
    let test_host = format!("dip-doctor-probe.{tld}");

    // Use the OS resolver — this validates the full chain
    // (macOS /etc/resolver, Linux systemd-resolved)
    use std::net::ToSocketAddrs;
    match format!("{test_host}:80").to_socket_addrs() {
        Ok(mut addrs) => {
            let resolved: Vec<_> = addrs.by_ref().map(|a| a.ip()).collect();
            if resolved.iter().any(|ip| ip.to_string() == "127.0.0.1") {
                Check::ok("DNS", format!("{test_host} → 127.0.0.1"))
            } else {
                let got = resolved
                    .iter()
                    .map(|i| i.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                Check::warn(
                    "DNS",
                    format!("{test_host} → {got} (expected 127.0.0.1)"),
                    "run: dip proxy init",
                )
            }
        }
        Err(_) => Check::fail(
            "DNS",
            format!("{test_host} did not resolve"),
            "run: dip proxy init",
        ),
    }
}

fn check_ports(cfg: &config::ProxyConfig) -> Check {
    use crate::commands::proxy::daemon_pid;

    // Only warn about ports when proxy is NOT running — if it is running, it owns them
    if daemon_pid().is_some() {
        return Check::ok(
            "Ports",
            format!(":{} :{} owned by proxy", cfg.http_port, cfg.https_port),
        );
    }

    let mut blocked = vec![];
    for port in [cfg.http_port, cfg.https_port] {
        if !port_is_free(port) {
            blocked.push(port);
        }
    }

    if blocked.is_empty() {
        Check::ok(
            "Ports",
            format!(":{} :{} are free", cfg.http_port, cfg.https_port),
        )
    } else {
        let ports = blocked
            .iter()
            .map(|p| format!(":{p}"))
            .collect::<Vec<_>>()
            .join(", ");
        Check::warn(
            "Ports",
            format!("{ports} already in use"),
            "stop the conflicting process, then: dip proxy start",
        )
    }
}

#[cfg(target_os = "linux")]
fn check_linux_caps(cfg: &config::ProxyConfig) -> Check {
    if cfg.dns_port >= 1024 {
        return Check::ok(
            "Capabilities",
            format!("DNS on port {} — no cap needed", cfg.dns_port),
        );
    }

    let Ok(exe) = std::env::current_exe() else {
        return Check::warn(
            "Capabilities",
            "could not determine binary path",
            "run: dip proxy init",
        );
    };

    let output = std::process::Command::new("getcap").arg(&exe).output();

    match output {
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            if text.contains("cap_net_bind_service") {
                Check::ok(
                    "Capabilities",
                    format!("cap_net_bind_service set on {}", exe.display()),
                )
            } else {
                Check::fail(
                    "Capabilities",
                    format!("cap_net_bind_service missing (DNS port {})", cfg.dns_port),
                    "run: dip proxy init",
                )
            }
        }
        Err(_) => Check::warn(
            "Capabilities",
            "`getcap` not found — cannot verify",
            "install libcap or run: dip proxy init",
        ),
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn cert_days_left(path: &std::path::Path) -> Option<i64> {
    use x509_parser::prelude::*;
    let pem = std::fs::read_to_string(path).ok()?;
    let (_, pem_obj) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).ok()?;
    let (_, cert) = X509Certificate::from_der(&pem_obj.contents).ok()?;
    let not_after = cert.validity().not_after.timestamp();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((not_after - now) / 86_400)
}

fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("0.0.0.0", port)).is_ok()
}
