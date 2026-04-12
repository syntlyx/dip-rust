use anyhow::Result;
use colored::Colorize;

use crate::project::ProjectConfig;
use crate::proxy::config as proxy_config;
use crate::utils::output::Output;

pub fn run(port: Option<u16>, service: Option<&str>, verbose: bool, no_color: bool) -> Result<()> {
    let out = Output::new(no_color);

    // --port overrides auto-detection; upstream is 127.0.0.1:port in that case
    let (upstream, label) = if let Some(p) = port {
        (format!("127.0.0.1:{p}"), format!("localhost:{p}"))
    } else {
        detect_upstream(service, verbose)?
    };

    out.section("dip share", || {
        println!("  Tunneling {} → localhost.run", label.cyan());
        println!("  Press {} to stop\n", "Ctrl+C".dimmed());
    });

    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(crate::tunnel::run_tunnel(&upstream, &out))
}

/// Detect the upstream to tunnel by reading the proxy config.
///
/// The proxy already knows container IP:port for each domain — no docker inspect needed.
/// Returns (upstream, display_label).
fn detect_upstream(service: Option<&str>, _verbose: bool) -> Result<(String, String)> {
    let project = ProjectConfig::load()?;
    let env = project.get_env();
    let domain = env.get("DOMAIN").cloned().unwrap_or_default();

    let routes = proxy_config::load().map(|c| c.routes).unwrap_or_default();

    if routes.is_empty() {
        anyhow::bail!(
            "No proxy routes found — run `dip start` first, or use --port to specify the port manually"
        );
    }

    let matching: Vec<_> = routes
        .iter()
        .filter(|r| {
            if let Some(svc) = service {
                r.domain.contains(svc)
            } else {
                r.domain == domain || r.domain.ends_with(&format!(".{domain}"))
            }
        })
        .collect();

    let route = match matching.len() {
        0 => anyhow::bail!("No proxy route for '{domain}' — run `dip start` first, or use --port"),
        1 => matching[0],
        _ => {
            // Multiple routes — pick the one whose domain exactly equals DOMAIN,
            // or the shortest (most likely the main entry point)
            matching
                .iter()
                .find(|r| r.domain == domain)
                .copied()
                .unwrap_or_else(|| matching.iter().min_by_key(|r| r.domain.len()).unwrap())
        }
    };

    Ok((route.upstream.clone(), route.domain.clone()))
}
