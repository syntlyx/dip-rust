use crate::utils::style::Stylize;
use anyhow::Result;

use crate::commands::ctx::Ctx;
use crate::utils::output::service_color;

// ─── predefined filter keyword sets ──────────────────────────────────────────

const ERRORS: &[&str] = &["error", "exception", "fatal", "panic", "critical"];
const WARN: &[&str] = &["warn", "warning"];
const SQL: &[&str] = &["select ", "insert ", "update ", "delete ", "query "];
const HTTP: &[&str] = &[
    "get /", "post /", "put /", "delete /", "patch /", " 200 ", " 201 ", " 204 ", " 301 ", " 302 ",
    " 400 ", " 401 ", " 403 ", " 404 ", " 422 ", " 500 ", " 502 ", " 503 ",
];
const SLOW: &[&str] = &["slow", "timeout", "exceeded", "deadline", "timed out"];

// ─── public entry point ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub fn run(
    service: Option<String>,
    since: Option<String>,
    grep: Option<String>,
    errors: bool,
    warn: bool,
    sql: bool,
    http: bool,
    slow: bool,
    verbose: bool,
    no_color: bool,
) -> Result<()> {
    let ctx = Ctx::load(verbose, no_color)?;

    let header = match &service {
        Some(s) => format!("  {} {}", "logs".dimmed(), s.color(service_color(s)).bold()),
        None => format!(
            "  {} {}",
            "logs".dimmed(),
            ctx.rt.project.project_name.bold()
        ),
    };
    println!("{}", "─".repeat(50));
    println!("{header}");
    println!("{}", "─".repeat(50));

    // Show available services if no specific one requested.
    #[allow(clippy::collapsible_if)]
    if service.is_none() && !no_color {
        if let Ok(raw) = ctx.rt.compose_capture(&["config", "--services"]) {
            let services: Vec<&str> = raw
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            if !services.is_empty() {
                let colored: Vec<String> = services
                    .iter()
                    .map(|s| s.color(service_color(s)).bold().to_string())
                    .collect();
                println!("  {}", colored.join("  "));
                println!("{}", "─".repeat(50));
            }
        }
    }

    // Build compose args.
    let mut args = vec!["logs", "-f", "--tail=100"];
    let since_arg;
    if let Some(ref s) = since {
        since_arg = format!("--since={s}");
        args.push(since_arg.as_str());
    }
    if let Some(ref svc) = service {
        args.push(svc.as_str());
    }

    // Resolve keywords from flags + --grep.
    let keywords = resolve_keywords(grep.as_deref(), errors, warn, sql, http, slow);

    if let Some(ref kws) = keywords {
        // Print active filters in the header.
        let active = active_label(grep.as_deref(), errors, warn, sql, http, slow);
        println!("  {} {}", "filter:".dimmed(), active.yellow().bold());
        println!("{}", "─".repeat(50));
        ctx.out.info("Ctrl+C to exit");
        let kw_refs: Vec<&str> = kws.iter().map(String::as_str).collect();
        ctx.rt.compose_stream_grep(&args, &kw_refs)?;
    } else {
        ctx.out.info("Ctrl+C to exit");
        ctx.rt.compose_stream(&args, false)?;
    }

    Ok(())
}

// ─── helpers ─────────────────────────────────────────────────────────────────

fn resolve_keywords(
    grep: Option<&str>,
    errors: bool,
    warn: bool,
    sql: bool,
    http: bool,
    slow: bool,
) -> Option<Vec<String>> {
    let mut kws: Vec<String> = Vec::new();

    if let Some(p) = grep {
        kws.push(p.to_lowercase());
    }
    if errors {
        kws.extend(ERRORS.iter().map(|s| s.to_string()));
    }
    if warn {
        kws.extend(WARN.iter().map(|s| s.to_string()));
    }
    if sql {
        kws.extend(SQL.iter().map(|s| s.to_string()));
    }
    if http {
        kws.extend(HTTP.iter().map(|s| s.to_string()));
    }
    if slow {
        kws.extend(SLOW.iter().map(|s| s.to_string()));
    }

    if kws.is_empty() { None } else { Some(kws) }
}

fn active_label(
    grep: Option<&str>,
    errors: bool,
    warn: bool,
    sql: bool,
    http: bool,
    slow: bool,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = grep {
        parts.push(format!("\"{}\"", p));
    }
    if errors {
        parts.push("--errors".into());
    }
    if warn {
        parts.push("--warn".into());
    }
    if sql {
        parts.push("--sql".into());
    }
    if http {
        parts.push("--http".into());
    }
    if slow {
        parts.push("--slow".into());
    }
    parts.join(" + ")
}
