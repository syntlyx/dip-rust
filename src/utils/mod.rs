pub mod output;

use anyhow::Result;

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
