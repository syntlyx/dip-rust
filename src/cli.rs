use std::path::PathBuf;

use clap::{Parser, Subcommand};
use clap_complete::Shell;

#[derive(Parser)]
#[command(
    name = "dip",
    about = "Docker Integration Platform - Simplify Docker workflows"
)]
#[command(author, version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Enable verbose output for debugging
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Disable colored output
    #[arg(long, global = true)]
    pub no_color: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Initialize a new dip project in the current directory
    Init {
        /// Project template: nestjs, nextjs, node, laravel
        #[arg(long, short = 't')]
        template: Option<String>,
    },

    /// List all dip projects found on this machine
    Ls {
        /// Root directory to scan (defaults to home directory)
        #[arg(long)]
        root: Option<std::path::PathBuf>,
    },

    /// Show resolved project environment variables
    Env,

    /// Open the project URL in the default browser
    Open {
        /// Service name or domain fragment (opens main domain if omitted)
        service: Option<String>,
    },

    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },

    /// Show system and Docker environment information
    Sysinfo,

    /// Show TLS certificate info (validity, SANs, keychain trust)
    Cert,

    // ── Container lifecycle ────────────────────────────────────────────────
    /// Start all project containers (runs pre-start hook if present)
    Start,
    /// Stop all project containers
    Stop,
    /// Restart containers — stop then up -d (picks up new env vars)
    Restart,
    /// Build or rebuild service images
    Build {
        /// Specific service to build (builds all if omitted)
        service: Option<String>,
    },
    /// Pull latest Docker images for project services
    Pull,
    /// Hard reset: stop, remove, and start again
    Reset,
    /// Remove containers (and optionally a specific service)
    Remove {
        /// Specific service to remove (removes all if omitted)
        service: Option<String>,
    },
    /// Remove stopped containers and dangling images for this project
    Cleanup,

    // ── Container info ─────────────────────────────────────────────────────
    /// Show status of project containers
    Status,
    /// Stream container logs
    Logs {
        /// Specific service to tail (tails all if omitted)
        service: Option<String>,
    },
    /// Show resource usage (CPU / memory / I/O)
    Stats {
        /// Specific service (shows all if omitted)
        service: Option<String>,
    },
    /// Show running processes inside containers
    Top {
        /// Specific service (shows all if omitted)
        service: Option<String>,
    },
    /// Run health checks on all services
    Health,

    // ── Shell / exec ───────────────────────────────────────────────────────
    /// Open an interactive shell in a running container
    Shell {
        /// Service name
        service: String,
        /// Shell type (bash, sh, zsh, fish)
        #[arg(short = 't', long = "type", default_value = "bash")]
        shell_type: String,
    },
    /// Open a bash shell (alias for: shell --type bash)
    Bash {
        /// Service name
        service: String,
    },
    /// Execute a command inside a service container
    Exec {
        /// Service name
        service: String,
        /// Command and arguments to run
        #[arg(required = true, trailing_var_arg = true)]
        command: Vec<String>,
    },

    // ── Custom scripts ─────────────────────────────────────────────────────
    /// Run a script from .dip/commands/
    Run {
        /// Script name (file in .dip/commands/)
        script: String,
        /// Arguments passed to the script
        #[arg(trailing_var_arg = true)]
        args: Vec<String>,
    },

    // ── Built-in reverse proxy ─────────────────────────────────────────────
    /// Manage the built-in TLS reverse proxy
    #[command(subcommand)]
    Proxy(ProxyCommands),

    // ── Database ───────────────────────────────────────────────────────────
    /// Database operations
    #[command(subcommand)]
    Db(DbCommands),

    // ── System ─────────────────────────────────────────────────────────────
    /// Remove all unused Docker system resources (with confirmation)
    Prune,
    /// Update dip to the latest version
    Update,
}

#[derive(Subcommand)]
pub enum DbCommands {
    /// List detected database services
    List,
    /// Export the project database to a dump file
    Dump {
        /// Output file path
        output_path: PathBuf,
        /// Service name when multiple DB services are defined (e.g. --service mysql)
        #[arg(short, long)]
        service: Option<String>,
    },
    /// Import a database dump into the project database
    Import {
        /// Input file path (.sql)
        input_path: PathBuf,
        /// Service name when multiple DB services are defined (e.g. --service mysql)
        #[arg(short, long)]
        service: Option<String>,
    },
}

// ─── Built-in proxy subcommands ───────────────────────────────────────────────

#[derive(Subcommand)]
pub enum ProxyCommands {
    /// Generate CA + wildcard cert, install CA in system keychain (run once)
    Init,
    /// Start the proxy daemon (may require sudo for ports 80/443)
    Start,
    /// Stop the proxy daemon
    Stop,
    /// Restart the proxy daemon
    Restart,
    /// Show daemon status
    Status,
    /// Tail the proxy access log
    Logs {
        /// Number of recent lines to show before following (default: 50)
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,
    },
    /// List all routing rules
    Routes,
    /// Sync routes from running docker-compose containers (reads `host` labels)
    Sync,
    /// Add or update a routing rule manually (exact domain takes priority over wildcard)
    Add {
        /// Domain pattern, e.g. "api.myapp.test" or "*.myapp.test"
        domain: String,
        /// Upstream address, e.g. "127.0.0.1:3000" or "container-name:8080"
        upstream: String,
    },
    /// Remove a routing rule by domain
    Remove {
        /// Domain pattern to remove
        domain: String,
    },
    /// Show or update DNS / port configuration
    Config {
        /// TLD(s) to resolve → 127.0.0.1, comma-separated (e.g. "test,local")
        #[arg(long)]
        tld: Option<String>,
        /// Port the built-in DNS server listens on (must match /etc/resolver files)
        #[arg(long)]
        dns_port: Option<u16>,
    },
    /// Run the proxy server in the foreground (used internally by daemon)
    #[command(hide = true)]
    Serve,
}
