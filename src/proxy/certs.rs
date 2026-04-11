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

/// Ensure the server cert covers all TLDs found in `domains`.
/// Automatically derives wildcards: "api.foo.test" → "*.foo.test".
/// Returns `true` if the cert was (re-)generated (proxy needs restart in that case).
pub fn ensure_server_cert(domains: &[String]) -> Result<bool> {
    let needed = wildcard_sans(domains);

    // If the caller passed an empty domain list but a cert already exists,
    // leave it alone — a real cert from a previous sync is better than localhost.
    if needed.is_empty() && srv_cert_path().exists() {
        return Ok(false);
    }

    if srv_cert_path().exists() && !cert_needs_regen(&needed) {
        return Ok(false);
    }

    let (ca_cert, ca_key) = ensure_ca()?;
    let srv_key = KeyPair::generate()?;

    // Build full SAN list: wildcards + exact non-wildcard domains
    let mut sans: Vec<String> = needed.iter().cloned().collect();
    for d in domains {
        if !d.contains('*') && !sans.contains(d) {
            sans.push(d.clone());
        }
    }
    if sans.is_empty() {
        sans.push("localhost".to_string());
    }

    eprintln!("dip-proxy: generating cert with SANs: {}", sans.join(", "));

    let mut params = CertificateParams::new(sans.clone())?;
    // Use the first SAN as CN so the cert is clearly identifiable in Keychain
    let cn = sans
        .first()
        .cloned()
        .unwrap_or_else(|| "dip local".to_string());
    params.distinguished_name = server_dn(&cn);
    // Chrome/Safari cap TLS cert validity at 398 days even for private CAs.
    // 397 days is the safe maximum.
    let now = OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(397);
    // Proper key usage for a TLS server cert
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
            cert_path.to_str().unwrap(),
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
    // Linux: print instructions; automated install varies too much by distro
    eprintln!(
        "Install the CA manually:\n  sudo cp {} /usr/local/share/ca-certificates/dip-ca.crt\n  sudo update-ca-certificates",
        ca_cert_path().display()
    );
    Ok(false)
}

// ─── internals ───────────────────────────────────────────────────────────────

/// Derive minimal wildcard SANs from a list of domain patterns.
///
///   "api.foo.test"    →  "*.foo.test"
///   "*.bar.test"      →  "*.bar.test"
///   "a.b.c.test"      →  "*.b.c.test"
fn wildcard_sans(domains: &[String]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for d in domains {
        if d.starts_with("*.") {
            out.insert(d.clone());
        } else if let Some(dot) = d.find('.') {
            out.insert(format!("*.{}", &d[dot + 1..]));
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
