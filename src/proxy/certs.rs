/// OrbStack-style certificate management:
///   - One CA cert generated once, installed in system keychain
///   - One wildcard server cert that covers all configured TLDs
///   - Auto-regenerated when new domains are added
///
/// Files (all in ~/.config/dip/proxy/):
///   ca.pem      — CA certificate (import into system trust store)
///   ca.key      — CA private key
///   server.pem  — server cert chain (domain cert + CA)
///   server.key  — server private key
///   server.sans — list of SANs the current server cert covers (used to detect regen need)
use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::Result;
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType, DnValue, IsCa,
    KeyPair, KeyUsagePurpose,
};
use time::OffsetDateTime;

use crate::dirs;

// ─── paths ───────────────────────────────────────────────────────────────────

pub fn ca_cert_path() -> PathBuf {
    dirs::proxy_dir().join("ca.pem")
}
pub fn ca_key_path() -> PathBuf {
    dirs::proxy_dir().join("ca.key")
}
pub fn srv_cert_path() -> PathBuf {
    dirs::proxy_dir().join("server.pem")
}
pub fn srv_key_path() -> PathBuf {
    dirs::proxy_dir().join("server.key")
}
fn sans_file() -> PathBuf {
    dirs::proxy_dir().join("server.sans")
}

// ─── public API ──────────────────────────────────────────────────────────────

/// Ensure CA exists. Creates it on first call.
/// Returns (cert, keypair) for signing operations.
pub fn ensure_ca() -> Result<(Certificate, KeyPair)> {
    let cert_path = ca_cert_path();
    let key_path = ca_key_path();

    if cert_path.exists() && key_path.exists() {
        let key_pem = std::fs::read_to_string(&key_path)?;
        let cert_pem = std::fs::read_to_string(&cert_path)?;
        let key_pair = KeyPair::from_pem(&key_pem)?;
        let params = CertificateParams::from_ca_cert_pem(&cert_pem)?;
        let cert = params.self_signed(&key_pair)?;
        return Ok((cert, key_pair));
    }

    std::fs::create_dir_all(dirs::proxy_dir())?;

    let key_pair = KeyPair::generate()?;
    let mut params = CertificateParams::new(vec![])?;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.distinguished_name = ca_dn();
    // CA valid for 20 years — browsers don't cap CA cert validity
    let now = OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(7300);

    let cert = params.self_signed(&key_pair)?;
    write_600(&key_path, key_pair.serialize_pem().as_bytes())?;
    std::fs::write(&cert_path, cert.pem())?;

    Ok((cert, key_pair))
}

/// Ensure the server cert covers all domains.
///
/// Strategy:
///   - Domains with 3+ labels (e.g. `api.foo.test`)  → wildcard `*.foo.test`
///   - Domains with 2 labels  (e.g. `laravel.test`)   → exact SAN only
///     (browsers reject TLD wildcards like `*.test`)
///
/// Returns `true` if the cert was (re-)generated (proxy needs restart in that case).
pub fn ensure_server_cert(domains: &[String]) -> Result<bool> {
    // Wildcards only for sub-subdomains (3+ labels); never *.tld
    let wildcards = wildcard_sans(domains);

    // Exact SANs: any domain not fully covered by one of the wildcards above
    let mut exact: BTreeSet<String> = BTreeSet::new();
    for d in domains {
        if d.contains('*') {
            continue;
        }
        let covered = wildcards.iter().any(|w| domain_matches_wildcard(d, w));
        if !covered {
            exact.insert(d.clone());
        }
    }

    // The complete set we need the cert to cover (used for regen detection)
    let mut needed: BTreeSet<String> = wildcards.iter().cloned().collect();
    needed.extend(exact.iter().cloned());

    // Nothing to do yet but a cert exists — leave it alone
    if needed.is_empty() && srv_cert_path().exists() {
        return Ok(false);
    }

    if srv_cert_path().exists() && !cert_needs_regen(&needed) {
        return Ok(false);
    }

    let (ca_cert, ca_key) = ensure_ca()?;
    let srv_key = KeyPair::generate()?;

    let mut sans: Vec<String> = needed.iter().cloned().collect();
    if sans.is_empty() {
        sans.push("localhost".to_string());
    }

    eprintln!("dip-proxy: generating cert with SANs: {}", sans.join(", "));

    // Use the first exact (non-wildcard) domain as CN so it's readable in Keychain
    let cn = exact
        .iter()
        .next()
        .or_else(|| sans.first())
        .cloned()
        .unwrap_or_else(|| "dip local".to_string());

    let mut params = CertificateParams::new(sans.clone())?;
    params.distinguished_name = server_dn(&cn);
    // Chrome/Safari cap TLS cert validity at 398 days even for private CAs.
    // 397 days is the safe maximum.
    let now = OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(397);
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];

    let domain_cert = params.signed_by(&srv_key, &ca_cert, &ca_key)?;

    write_600(&srv_key_path(), srv_key.serialize_pem().as_bytes())?;
    // Chain = server cert + CA cert (browser needs both to verify the chain)
    std::fs::write(
        srv_cert_path(),
        format!("{}{}", domain_cert.pem(), ca_cert.pem()),
    )?;

    save_sans(&needed)?;
    Ok(true)
}

/// Returns true if `domain` is covered by the given wildcard SAN.
/// `*.foo.test` covers `bar.foo.test` but NOT `foo.test` itself.
fn domain_matches_wildcard(domain: &str, wildcard: &str) -> bool {
    if let Some(suffix) = wildcard.strip_prefix("*.")
        && let Some(dot) = domain.find('.')
    {
        return &domain[dot + 1..] == suffix;
    }
    false
}

/// Install the CA into the system trust store on macOS.
///
/// Always (re)installs the current CA cert — checking by name is not enough
/// because generating a new CA produces a different key pair, so the old trusted
/// cert in the Keychain would not validate certs signed by the new CA.
/// We remove any existing entry first to avoid stale trust anchors.
#[cfg(target_os = "macos")]
pub fn install_ca() -> Result<bool> {
    let cert_path = ca_cert_path();
    anyhow::ensure!(
        cert_path.exists(),
        "CA cert not found — run `dip proxy init` first"
    );

    // Remove stale entry (same name, potentially different key).
    // Suppress output — "Unable to delete certificate matching..." is expected when empty.
    let _ = std::process::Command::new("sudo")
        .args([
            "security",
            "delete-certificate",
            "-c",
            "dip Local CA",
            "/Library/Keychains/System.keychain",
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let cert_path_str = cert_path.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "CA cert path contains invalid UTF-8: {}",
            cert_path.display()
        )
    })?;

    // Add current CA and mark as a trusted root
    let status = std::process::Command::new("sudo")
        .args([
            "security",
            "add-trusted-cert",
            "-d", // add to admin cert store
            "-r",
            "trustRoot", // trust as a root CA for all purposes
            "-k",
            "/Library/Keychains/System.keychain",
            cert_path_str,
        ])
        .status()?;

    anyhow::ensure!(
        status.success(),
        "Failed to install CA into system keychain"
    );
    Ok(true)
}

#[cfg(not(target_os = "macos"))]
pub fn install_ca() -> Result<bool> {
    use std::process::Command;

    // Directories where CA certs can be placed — first existing one wins.
    // (dir, filename)
    let cert_dirs: &[(&str, &str)] = &[
        ("/usr/local/share/ca-certificates", "dip-ca.crt"), // Debian / Ubuntu / Gentoo
        ("/etc/pki/ca-trust/source/anchors", "dip-ca.crt"), // RHEL / Fedora / CentOS
        ("/etc/ca-certificates/trust-source/anchors", "dip-ca.crt"), // Arch
        ("/etc/ssl/certs", "dip-ca.pem"),                   // Gentoo / Alpine
    ];

    // Commands to refresh the system trust store — first available one is used.
    // Each entry is argv: ["cmd", "arg1", ...]
    let update_cmds: &[&[&str]] = &[
        &["update-ca-certificates"],
        &["update-ca-trust"],
        &["c_rehash", "/etc/ssl/certs"],
        &["openssl", "rehash", "/etc/ssl/certs"],
    ];

    let cert_path = ca_cert_path();

    // 1. Find the cert store directory
    let Some((dir, filename)) = cert_dirs
        .iter()
        .find(|(dir, _)| std::path::Path::new(dir).is_dir())
    else {
        let path = cert_path.display();
        eprintln!("No supported CA cert store found. Install manually:");
        eprintln!();
        eprintln!("  Debian/Ubuntu/Gentoo:");
        eprintln!("    sudo cp {path} /usr/local/share/ca-certificates/dip-ca.crt");
        eprintln!("    sudo update-ca-certificates");
        eprintln!();
        eprintln!("  RHEL/Fedora/CentOS:");
        eprintln!("    sudo cp {path} /etc/pki/ca-trust/source/anchors/dip-ca.crt");
        eprintln!("    sudo update-ca-trust");
        eprintln!();
        eprintln!("  Gentoo/Alpine:");
        eprintln!("    sudo cp {path} /etc/ssl/certs/dip-ca.pem");
        eprintln!("    sudo c_rehash /etc/ssl/certs");
        return Ok(false);
    };

    // 2. Copy the cert
    let dest = format!("{dir}/{filename}");
    let cert_path_str = cert_path.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "CA cert path contains invalid UTF-8: {}",
            cert_path.display()
        )
    })?;
    let ok = Command::new("sudo")
        .args(["cp", cert_path_str, &dest])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    anyhow::ensure!(ok, "Failed to copy CA cert to {dest}");

    // 3. Run the first available update command
    let updated = update_cmds.iter().find_map(|argv| {
        Command::new("sudo")
            .args(*argv)
            .status()
            .ok()
            .filter(|s| s.success())
            .map(|_| argv[0])
    });

    match updated {
        Some(cmd) => eprintln!("CA cert installed via {cmd}"),
        None => eprintln!(
            "Warning: cert copied to {dest} but no update command succeeded — run one manually"
        ),
    }

    Ok(true)
}

// ─── internals ───────────────────────────────────────────────────────────────

/// Derive minimal wildcard SANs from a list of domain patterns.
///
///   "api.foo.test"    →  "*.foo.test"   (3 labels — safe wildcard)
///   "*.bar.test"      →  "*.bar.test"   (pass-through)
///   "a.b.c.test"      →  "*.b.c.test"
///   "laravel.test"    →  (nothing)      — 2 labels, TLD wildcard (*.test) rejected by browsers
fn wildcard_sans(domains: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for d in domains {
        if d.starts_with("*.") {
            out.insert(d.clone());
        } else {
            // Only generate a wildcard when the result covers at least 2 labels
            // (i.e. the original domain has 3+ labels: sub.foo.tld → *.foo.tld).
            // "laravel.test" has only 2 labels → skip, will be added as exact SAN instead.
            let label_count = d.split('.').count();
            if label_count >= 3 {
                // label_count >= 3 guarantees a '.' exists; skip if somehow not found.
                if let Some(dot) = d.find('.') {
                    out.insert(format!("*.{}", &d[dot + 1..]));
                }
            }
        }
    }
    out
}

/// True if the server cert doesn't already cover all `needed` SANs.
fn cert_needs_regen(needed: &BTreeSet<String>) -> bool {
    let stored: BTreeSet<String> = std::fs::read_to_string(sans_file())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    &stored != needed
}

fn save_sans(sans: &BTreeSet<String>) -> Result<()> {
    let content: String = sans.iter().cloned().collect::<Vec<_>>().join("\n");
    std::fs::write(sans_file(), content)?;
    Ok(())
}

fn ca_dn() -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(
        DnType::CountryName,
        DnValue::PrintableString("US".try_into().unwrap()),
    );
    dn.push(DnType::StateOrProvinceName, "Local");
    dn.push(DnType::LocalityName, "Local Development");
    dn.push(DnType::OrganizationName, "dip");
    dn.push(DnType::OrganizationalUnitName, "Certificate Authority");
    dn.push(DnType::CommonName, "dip Local CA");
    dn
}

fn server_dn(cn: &str) -> DistinguishedName {
    let mut dn = DistinguishedName::new();
    dn.push(
        DnType::CountryName,
        DnValue::PrintableString("US".try_into().unwrap()),
    );
    dn.push(DnType::StateOrProvinceName, "Local");
    dn.push(DnType::LocalityName, "Local Development");
    dn.push(DnType::OrganizationName, "dip");
    dn.push(DnType::OrganizationalUnitName, "Local Proxy");
    dn.push(DnType::CommonName, cn);
    dn
}

fn write_600(path: &PathBuf, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    Ok(())
}
