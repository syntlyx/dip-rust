pub mod output;

use std::path::Path;

use anyhow::Result;

/// Ensure a file has executable bits set (`chmod +x`).
/// No-op if the file is already executable.
pub fn ensure_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path)?;
    let mode = meta.permissions().mode();
    if mode & 0o111 == 0 {
        let mut perms = meta.permissions();
        perms.set_mode(mode | 0o755);
        std::fs::set_permissions(path, perms)?;
    }
    Ok(())
}

/// Prompt the user for a yes/no confirmation. Returns `true` for "y" / "yes".
pub fn confirm(prompt: &str) -> Result<bool> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// Parse newline-delimited JSON lines into typed structs, silently skipping
/// empty lines. Parse errors are only reported when `verbose` is true.
pub fn parse_jsonl<T>(raw: &str, verbose: bool) -> Vec<T>
where
    T: serde::de::DeserializeOwned,
{
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<T>(line) {
            Ok(v) => out.push(v),
            Err(e) => {
                if verbose {
                    eprintln!("Failed to parse line: {e}");
                }
            }
        }
    }
    out
}
