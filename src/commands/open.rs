use anyhow::Result;
use colored::Colorize;

use crate::project::ProjectConfig;
use crate::proxy::config as proxy_config;
use crate::utils::output::Output;

/// `dip open [service]`
///
/// Opens the project URL in the default browser.
///
///   dip open           → https://{DOMAIN}  (from .env)
///   dip open api       → first proxy route whose domain contains "api"
///   dip open           → if multiple routes exist, lists them all
pub fn run(service: Option<&str>, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);
    let project = ProjectConfig::load()?;
    let env = project.get_env();

    let url = if let Some(svc) = service {
        // Find a matching route in the proxy config
        url_for_service(svc)?
    } else {
        // No arg — use DOMAIN from .env or list all routes
        let routes = proxy_config::load().map(|c| c.routes).unwrap_or_default();

        match routes.len() {
            0 => {
                // Fall back to DOMAIN env var
                let domain = env
                    .get("DOMAIN")
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("DOMAIN not set — run inside a dip project"))?;
                format!("https://{domain}")
            }
            1 => format!("https://{}", routes[0].domain),
            _ => {
                // Multiple routes — let the user pick
                return pick_and_open(&routes, &out);
            }
        }
    };

    open_url(&url, &out)
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Find the best-matching proxy route for a service name and return its URL.
fn url_for_service(svc: &str) -> Result<String> {
    let routes = proxy_config::load()?.routes;

    // Exact domain match first
    if let Some(r) = routes.iter().find(|r| r.domain == svc) {
        return Ok(format!("https://{}", r.domain));
    }

    // Substring match on domain (e.g. "api" matches "api.myapp.test")
    let matches: Vec<_> = routes.iter().filter(|r| r.domain.contains(svc)).collect();

    match matches.len() {
        0 => anyhow::bail!("No proxy route matching '{svc}' — run `dip proxy routes` to see all"),
        1 => Ok(format!("https://{}", matches[0].domain)),
        _ => {
            // Multiple matches — pick the shortest (most specific) domain
            let best = matches.iter().min_by_key(|r| r.domain.len()).unwrap();
            Ok(format!("https://{}", best.domain))
        }
    }
}

/// Print a numbered list of routes and open the chosen one.
fn pick_and_open(routes: &[crate::proxy::config::Route], out: &Output) -> Result<()> {
    println!("  Multiple routes — choose one:\n");
    for (i, r) in routes.iter().enumerate() {
        println!("  {}  https://{}", (i + 1).to_string().dimmed(), r.domain);
    }
    println!();

    use std::io::{BufRead, Write};
    print!("  Open [1-{}]: ", routes.len());
    std::io::stdout().flush()?;

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;

    let n: usize = line.trim().parse().unwrap_or(0);
    if n == 0 || n > routes.len() {
        anyhow::bail!("Invalid selection");
    }

    let url = format!("https://{}", routes[n - 1].domain);
    open_url(&url, out)
}

/// Open a URL in the default browser.
fn open_url(url: &str, out: &Output) -> Result<()> {
    use colored::Colorize;
    out.info(&format!("Opening {}", url.cyan()));

    #[cfg(target_os = "macos")]
    let cmd = ("open", vec![url]);

    #[cfg(target_os = "linux")]
    let cmd = ("xdg-open", vec![url]);

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        out.warning("Cannot open browser automatically on this platform");
        println!("  {url}");
        return Ok(());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let status = std::process::Command::new(cmd.0)
            .args(&cmd.1)
            .status()
            .map_err(|e| anyhow::anyhow!("Failed to open browser: {e}"))?;

        if !status.success() {
            anyhow::bail!("Browser launch failed");
        }
    }

    Ok(())
}
