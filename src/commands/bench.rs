use std::cmp::Ordering;

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;

use crate::project::ProjectConfig;
use crate::runtime::{self, RuntimeBenchMeasurement, RuntimeBenchMount, RuntimeBenchSpec};
use crate::utils::output::Output;

#[derive(Serialize)]
struct RuntimeBenchRow {
    runtime: String,
    image: String,
    iterations: usize,
    warmup: usize,
    start_ms: f64,
    start_total_ms: f64,
    exec_ms: f64,
    exec_total_ms: f64,
    disk_ms: f64,
    disk_total_ms: f64,
    disk_mib_per_s: f64,
    total_ms: f64,
}

impl From<RuntimeBenchMeasurement> for RuntimeBenchRow {
    fn from(value: RuntimeBenchMeasurement) -> Self {
        Self {
            runtime: value.runtime,
            image: value.image,
            iterations: value.iterations,
            warmup: value.warmup,
            start_ms: value.start_ms,
            start_total_ms: value.start_total_ms,
            exec_ms: value.exec_ms,
            exec_total_ms: value.exec_total_ms,
            disk_ms: value.disk_ms,
            disk_total_ms: value.disk_total_ms,
            disk_mib_per_s: value.disk_mib_per_s,
            total_ms: value.total_ms,
        }
    }
}

pub struct SteadyBenchOptions {
    pub iterations: usize,
    pub warmup: usize,
    pub image: String,
    pub path: Option<String>,
    pub project_io: bool,
    pub size_mb: u64,
    pub json: bool,
    pub no_color: bool,
}

pub fn run_runtime(
    iterations: usize,
    warmup: usize,
    image: String,
    path: String,
    size_mb: u64,
    json: bool,
    no_color: bool,
) -> Result<()> {
    let spec = RuntimeBenchSpec {
        iterations: iterations.max(1),
        warmup,
        image,
        path,
        size_mb,
        mount: None,
    };
    run_bench(spec, BenchScenario::Lifecycle, None, json, no_color)
}

pub fn run_project_io(
    iterations: usize,
    warmup: usize,
    image: String,
    path: String,
    size_mb: u64,
    json: bool,
    no_color: bool,
) -> Result<()> {
    let project = ProjectConfig::load()?;
    let spec = RuntimeBenchSpec {
        iterations: iterations.max(1),
        warmup,
        image,
        path,
        size_mb,
        mount: Some(RuntimeBenchMount {
            host_path: project.root_dir,
            container_path: "/workspace".to_string(),
        }),
    };
    run_bench(
        spec,
        BenchScenario::Lifecycle,
        Some("project bind mount /workspace"),
        json,
        no_color,
    )
}

pub fn run_steady(options: SteadyBenchOptions) -> Result<()> {
    let mount = if options.project_io {
        let project = ProjectConfig::load()?;
        Some(RuntimeBenchMount {
            host_path: project.root_dir,
            container_path: "/workspace".to_string(),
        })
    } else {
        None
    };
    let path = options.path.unwrap_or_else(|| {
        if options.project_io {
            "/workspace/.dip-bench.bin".to_string()
        } else {
            "/tmp/dip-bench.bin".to_string()
        }
    });
    let spec = RuntimeBenchSpec {
        iterations: options.iterations.max(1),
        warmup: options.warmup,
        image: options.image,
        path,
        size_mb: options.size_mb,
        mount,
    };
    run_bench(
        spec,
        BenchScenario::Steady,
        options
            .project_io
            .then_some("project bind mount /workspace"),
        options.json,
        options.no_color,
    )
}

#[derive(Clone, Copy)]
enum BenchScenario {
    Lifecycle,
    Steady,
}

fn run_bench(
    spec: RuntimeBenchSpec,
    scenario: BenchScenario,
    mode: Option<&str>,
    json: bool,
    no_color: bool,
) -> Result<()> {
    let out = Output::new(no_color);
    if !json {
        match scenario {
            BenchScenario::Lifecycle => out.info(&format!(
                "Benchmarking disposable {} container(s); {} measured run(s), {} warmup",
                spec.image, spec.iterations, spec.warmup
            )),
            BenchScenario::Steady => out.info(&format!(
                "Benchmarking steady-state {}; 1 container start, {} measured loop(s), {} warmup",
                spec.image, spec.iterations, spec.warmup
            )),
        }
        if let Some(mode) = mode {
            out.info(mode);
        }
        out.info(&format!(
            "disk sample: {} MiB at {}",
            spec.size_mb, spec.path
        ));
        if runtime::known_runtime_names().len() > 1 {
            out.info(
                "Docker means whichever Docker-compatible engine is active, including OrbStack",
            );
        } else {
            out.info("Benchmarking the available Docker-compatible runtime");
        }
    }

    let mut rows = Vec::new();
    let mut skipped = Vec::new();
    for runtime_name in runtime::known_runtime_names().iter().copied() {
        let backend = runtime::backend_for_name(runtime_name)?;
        if let Err(e) = backend.check_daemon() {
            skipped.push((runtime_name.to_string(), e.to_string()));
            continue;
        }

        let result = match scenario {
            BenchScenario::Lifecycle => backend.bench(&spec),
            BenchScenario::Steady => backend.bench_steady(&spec),
        };

        match result {
            Ok(row) => rows.push(RuntimeBenchRow::from(row)),
            Err(e) => skipped.push((runtime_name.to_string(), e.to_string())),
        }
    }

    if rows.is_empty() {
        let detail = skipped
            .into_iter()
            .map(|(runtime, error)| format!("{runtime}: {error}"))
            .collect::<Vec<_>>()
            .join("; ");
        anyhow::bail!("No container runtime was available for benchmarking ({detail})");
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        print_table(&rows, scenario);
        for (runtime, error) in skipped {
            println!("  {} {}", format!("{runtime}:").dimmed(), error.dimmed());
        }
    }

    Ok(())
}

fn print_table(rows: &[RuntimeBenchRow], scenario: BenchScenario) {
    let start_label = match scenario {
        BenchScenario::Lifecycle => "start avg",
        BenchScenario::Steady => "start once",
    };
    println!("{}", "─".repeat(106));
    println!(
        "  {:<8} {:<16} {:>10} {:>10} {:>10} {:>12} {:>12}",
        "runtime", "image", start_label, "exec avg", "disk avg", "disk MiB/s", "total"
    );
    println!("{}", "─".repeat(106));
    for row in rows {
        println!(
            "  {:<8} {:<16} {:>10} {:>10} {:>10} {:>12} {:>12}",
            row.runtime,
            truncate(&row.image, 16),
            fmt_ms(row.start_ms),
            fmt_ms(row.exec_ms),
            fmt_ms(row.disk_ms),
            format!("{:.1}", row.disk_mib_per_s),
            fmt_ms(row.total_ms),
        );
    }
    println!("{}", "─".repeat(106));
    print_summary(rows, scenario);
}

#[derive(Clone, Copy)]
enum MetricDirection {
    Lower,
    Higher,
}

struct MetricWin<'a> {
    winner: &'a RuntimeBenchRow,
    runner_up: &'a RuntimeBenchRow,
    ratio: f64,
}

fn print_summary(rows: &[RuntimeBenchRow], scenario: BenchScenario) {
    if rows.len() < 2 {
        return;
    }

    println!("  summary:");
    let start_label = match scenario {
        BenchScenario::Lifecycle => "start avg",
        BenchScenario::Steady => "start once",
    };
    print_metric(
        start_label,
        rows,
        |row| row.start_ms,
        MetricDirection::Lower,
    );
    print_metric("exec avg", rows, |row| row.exec_ms, MetricDirection::Lower);
    print_metric(
        "disk I/O",
        rows,
        |row| row.disk_mib_per_s,
        MetricDirection::Higher,
    );
    print_metric("total", rows, |row| row.total_ms, MetricDirection::Lower);
}

fn print_metric<F>(label: &str, rows: &[RuntimeBenchRow], value: F, direction: MetricDirection)
where
    F: Fn(&RuntimeBenchRow) -> f64,
{
    let Some(win) = metric_win(rows, value, direction) else {
        return;
    };

    if win.ratio < 1.05 {
        println!(
            "    {label}: {} and {} are close",
            win.winner.runtime, win.runner_up.runtime
        );
    } else {
        println!(
            "    {label}: {} wins ({:.1}x over {})",
            win.winner.runtime, win.ratio, win.runner_up.runtime
        );
    }
}

fn metric_win<F>(
    rows: &[RuntimeBenchRow],
    value: F,
    direction: MetricDirection,
) -> Option<MetricWin<'_>>
where
    F: Fn(&RuntimeBenchRow) -> f64,
{
    let mut ranked = rows
        .iter()
        .filter_map(|row| {
            let metric = value(row);
            metric.is_finite().then_some((row, metric))
        })
        .filter(|(_, metric)| *metric > 0.0)
        .collect::<Vec<_>>();

    ranked.sort_by(|a, b| match direction {
        MetricDirection::Lower => a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal),
        MetricDirection::Higher => b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal),
    });

    let (winner, winner_value) = *ranked.first()?;
    let (runner_up, runner_up_value) = *ranked.get(1)?;
    let ratio = match direction {
        MetricDirection::Lower => runner_up_value / winner_value,
        MetricDirection::Higher => winner_value / runner_up_value,
    };

    Some(MetricWin {
        winner,
        runner_up,
        ratio,
    })
}

fn fmt_ms(value: f64) -> String {
    format!("{value:.1}ms")
}

fn truncate(value: &str, max: usize) -> String {
    if value.len() <= max {
        value.to_string()
    } else {
        format!("{}...", &value[..max.saturating_sub(3)])
    }
}
