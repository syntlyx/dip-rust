use crate::utils::style::Stylize;
use anyhow::Result;

use crate::proxy::certs;
use crate::utils::output::Output;

pub fn run(no_color: bool) -> Result<()> {
    let out = Output::new(no_color);

    let ca_path = certs::ca_cert_path();
    let srv_path = certs::srv_cert_path();

    if !ca_path.exists() {
        anyhow::bail!("No certificates found — run `dip proxy init` first");
    }

    let ca_info = read_cert_info(&ca_path)?;
    let srv_info = if srv_path.exists() {
        Some(read_cert_info(&srv_path)?)
    } else {
        None
    };

    let trusted = ca_is_trusted();

    out.section("certificates", || {
        // ── CA ────────────────────────────────────────────────────────────────
        println!("  {}", "CA certificate".bold());
        println!(
            "    {:<10} {}",
            "Path:".dimmed(),
            shorten_path(&ca_path).dimmed()
        );
        print_validity(ca_info.not_before_ts, ca_info.not_after_ts);

        let trust_str = if trusted {
            "installed in system keychain ✓".green().to_string()
        } else {
            "NOT in keychain — run `dip proxy init`".red().to_string()
        };
        println!("    {:<10} {}", "Keychain:".dimmed(), trust_str);

        // ── Server cert ───────────────────────────────────────────────────────
        println!();
        println!("  {}", "Server certificate".bold());

        if let Some(srv) = &srv_info {
            println!(
                "    {:<10} {}",
                "Path:".dimmed(),
                shorten_path(&srv_path).dimmed()
            );
            print_validity(srv.not_before_ts, srv.not_after_ts);

            if srv.sans.is_empty() {
                println!("    {:<10} {}", "SANs:".dimmed(), "none".dimmed());
            } else {
                println!(
                    "    {:<10} {}",
                    "SANs:".dimmed(),
                    srv.sans.join(", ").cyan()
                );
            }
        } else {
            println!(
                "    {}",
                "Not generated yet — run `dip start` inside a project".dimmed()
            );
        }
    });

    Ok(())
}

// ─── cert parsing ─────────────────────────────────────────────────────────────

struct CertInfo {
    not_before_ts: i64,
    not_after_ts: i64,
    sans: Vec<String>,
}

fn read_cert_info(path: &std::path::Path) -> Result<CertInfo> {
    use x509_parser::prelude::*;

    let pem = std::fs::read_to_string(path)?;

    // Parse only the first PEM block (server.pem has cert + CA chain)
    let (_, pem_obj) = x509_parser::pem::parse_x509_pem(pem.as_bytes())
        .map_err(|e| anyhow::anyhow!("PEM parse error: {e:?}"))?;

    let (_, cert) = X509Certificate::from_der(&pem_obj.contents)
        .map_err(|e| anyhow::anyhow!("DER parse error: {e:?}"))?;

    let not_before_ts = cert.validity().not_before.timestamp();
    let not_after_ts = cert.validity().not_after.timestamp();

    let sans: Vec<String> = if let Ok(Some(ext)) = cert.subject_alternative_name() {
        ext.value
            .general_names
            .iter()
            .filter_map(|san| match san {
                GeneralName::DNSName(s) => Some(s.to_string()),
                _ => None,
            })
            .collect()
    } else {
        vec![]
    };

    Ok(CertInfo {
        not_before_ts,
        not_after_ts,
        sans,
    })
}

// ─── display helpers ──────────────────────────────────────────────────────────

fn print_validity(not_before_ts: i64, not_after_ts: i64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let days_left = (not_after_ts - now) / 86_400;

    let not_before = format_ts(not_before_ts);
    let not_after = format_ts(not_after_ts);

    let expiry_str = if days_left < 0 {
        format!("EXPIRED {} days ago", -days_left).red().to_string()
    } else if days_left < 30 {
        format!("expires in {days_left} days").red().to_string()
    } else if days_left < 90 {
        format!("expires in {days_left} days").yellow().to_string()
    } else {
        format!("expires in {days_left} days").green().to_string()
    };

    println!(
        "    {:<10} {} → {}  ({})",
        "Valid:".dimmed(),
        not_before.dimmed(),
        not_after.dimmed(),
        expiry_str,
    );
}

fn format_ts(ts: i64) -> String {
    // Convert unix timestamp to YYYY-MM-DD without pulling in extra deps
    let secs = ts;
    // Days since unix epoch
    let days = secs / 86_400;
    // Simple Gregorian calendar conversion
    let (y, m, d) = days_to_ymd(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Convert days since 1970-01-01 to (year, month, day).
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

/// Check whether the dip CA is in the system keychain (macOS).
fn ca_is_trusted() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("security")
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
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "macos"))]
    false
}

fn shorten_path(path: &std::path::Path) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    path.to_string_lossy().replacen(&home, "~", 1)
}
