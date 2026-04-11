mod cli;
mod commands;
mod config;
mod db;
mod dirs;
mod dns;
mod hooks;
mod project;
mod proxy;
mod runtime;
mod templates;
mod utils;

use clap::Parser;
use cli::{Cli, Commands, DbCommands, ProxyCommands};
use colored::Colorize;

fn main() {
    let cli = Cli::parse();
    let v = cli.verbose;
    let nc = cli.no_color;

    let result = match cli.command {
        Commands::Init { template } => commands::init::run(template.as_deref(), nc),
        Commands::Ls { root } => commands::ls::run(root, nc),
        Commands::Env => commands::env::run(nc),
        Commands::Open { service } => commands::open::run(service.as_deref(), nc),
        Commands::Completions { shell } => commands::completions::run(shell),
        Commands::Sysinfo => commands::sysinfo::run(v, nc),
        Commands::Cert => commands::cert::run(nc),

        // Lifecycle
        Commands::Start => commands::start::run(v, nc),
        Commands::Stop => commands::stop::run(v, nc),
        Commands::Restart => commands::restart::run(v, nc),
        Commands::Build { service } => commands::build::run(service, v, nc),
        Commands::Pull => commands::pull::run(v, nc),
        Commands::Reset => commands::reset::run(v, nc),
        Commands::Remove { service } => commands::remove::run(service, v, nc),
        Commands::Cleanup => commands::cleanup::run(v, nc),

        // Info
        Commands::Status => commands::status::run(v, nc),
        Commands::Logs { service } => commands::logs::run(service, v, nc),
        Commands::Stats { service } => commands::stats::run(service, v, nc),
        Commands::Top { service } => commands::top::run(service, v, nc),
        Commands::Health => commands::health::run(v, nc),

        // Shell
        Commands::Shell {
            service,
            shell_type,
        } => commands::shell::run_shell(&service, &shell_type, v, nc),
        Commands::Bash { service } => commands::shell::run_shell(&service, "bash", v, nc),
        Commands::Exec { service, command } => commands::shell::run_exec(&service, &command, v, nc),

        // Custom scripts
        Commands::Run { script, args } => commands::run::run(&script, &args, v, nc),

        // Built-in proxy
        Commands::Proxy(sub) => match sub {
            ProxyCommands::Init => commands::proxy::run_init(nc),
            ProxyCommands::Start => commands::proxy::run_start(v, nc),
            ProxyCommands::Stop => commands::proxy::run_stop(nc),
            ProxyCommands::Restart => {
                commands::proxy::run_stop(nc).and_then(|_| commands::proxy::run_start(v, nc))
            }
            ProxyCommands::Status => commands::proxy::run_status(nc),
            ProxyCommands::Logs { lines } => commands::proxy::run_logs(lines, nc),
            ProxyCommands::Routes => commands::proxy::run_routes(nc),
            ProxyCommands::Sync => commands::proxy::run_sync(nc),
            ProxyCommands::Add { domain, upstream } => {
                commands::proxy::run_add(&domain, &upstream, nc)
            }
            ProxyCommands::Remove { domain } => commands::proxy::run_remove(&domain, nc),
            ProxyCommands::Config { tld, dns_port } => {
                commands::proxy::run_config(tld.as_deref(), dns_port, nc)
            }
            ProxyCommands::Serve => commands::proxy::run_serve(nc),
        },

        // Database
        Commands::Db(sub) => match sub {
            DbCommands::List => commands::db::run_list(v, nc),
            DbCommands::Dump {
                output_path,
                service,
            } => commands::db::run_dump(&output_path, service.as_deref(), v, nc),
            DbCommands::Import {
                input_path,
                service,
            } => commands::db::run_import(&input_path, service.as_deref(), v, nc),
        },

        // System
        Commands::Prune => commands::prune::run(nc),
        Commands::Update => commands::update::run(nc),
    };

    if let Err(e) = result {
        eprintln!("{} {e}", "✗".red());
        std::process::exit(1);
    }
}
