pub mod containers;
pub mod env;
pub mod notify;
pub mod output;
pub mod spinner;
pub mod style;
// Only the macOS Apple-runtime path parses compose files itself
// (`runtime::compose_file::load_project_compose`); elsewhere the module
// would be dead code. Tests exercise it on every platform.
#[cfg(any(target_os = "macos", test))]
pub mod yaml;

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

/// Current local wall-clock time formatted as `HH:MM:SS`, for log timestamps.
///
/// Goes through `libc::localtime_r` directly: `time::OffsetDateTime::now_local()`
/// refuses to determine the local UTC offset in multi-threaded processes (the
/// proxy runs inside tokio), and pulling in a date-time crate just for a log
/// prefix is not worth it.
pub fn local_hms() -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `libc::time(NULL)` only reads the system clock. `localtime_r` is
    // the thread-safe variant of `localtime`: it writes solely into the
    // caller-provided `tm` buffer (valid for the duration of the call) instead
    // of a shared static one.
    let ok = unsafe {
        let now = libc::time(std::ptr::null_mut());
        !libc::localtime_r(&now, &mut tm).is_null()
    };
    if !ok {
        return "??:??:??".into();
    }
    format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// Print `msg` to stderr only when `verbose` is true.
///
/// Prefer this over an inline `if verbose { eprintln!(...) }` block to keep
/// call sites clean. The message is only formatted when it will be printed.
#[inline]
pub fn log_verbose(verbose: bool, msg: &str) {
    if verbose {
        eprintln!("{msg}");
    }
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
