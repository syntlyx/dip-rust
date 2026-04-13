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
        /// Project template: nestjs, nextjs, node, laravel (shared base is always applied)
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

    /// Print the project root directory (exit 1 if not in a dip project)
    #[command(hide = true)]
    Root,

    /// Show system and Docker environment information
    Sysinfo,

    /// Show TLS certificate info (validity, SANs, keychain trust)
    Cert,

    /// Check system health: Docker, proxy, certs, DNS, ports
    Doctor,

    // ── Container lifecycle ────────────────────────────────────────────────
    /// Start project containers (runs pre-start hook if present)
    #[command(alias = "up")]
    Start {
        /// Specific service to start (starts all if omitted)
        service: Option<String>,
    },
    /// Stop project containers
    #[command(alias = "down")]
    Stop {
        /// Specific service to stop (stops all if omitted)
        service: Option<String>,
    },
    /// Restart containers — stop then up -d (picks up new env vars)
    #[command(alias = "reup")]
    Restart {
        /// Specific service to restart (restarts all if omitted)
        service: Option<String>,
    },
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
    Status {
        /// Output format: "json" for machine-readable output
        #[arg(long, value_name = "FORMAT")]
        format: Option<String>,
    },
    /// Alias for `status`
    #[command(hide = true)]
    Ps {
        /// Output format: "json" for machine-readable output
        #[arg(long, value_name = "FORMAT")]
        format: Option<String>,
    },
    /// Stream container logs
    Logs {
        /// Specific service to tail (tails all if omitted)
        service: Option<String>,
        /// Filter lines containing this pattern (case-insensitive)
        #[arg(long, short = 'g', value_name = "PATTERN")]
        grep: Option<String>,
        /// Show only error / exception / fatal / panic lines
        #[arg(long)]
        errors: bool,
        /// Show only warning lines
        #[arg(long)]
        warn: bool,
        /// Show only SQL query lines
        #[arg(long)]
        sql: bool,
        /// Show only HTTP request lines (GET/POST/… + status codes)
        #[arg(long)]
        http: bool,
        /// Show only slow / timeout / deadline lines
        #[arg(long)]
        slow: bool,
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
    /// Run a script from .dip/commands/ (no args = list all scripts)
    Run {
        /// Script name (file in .dip/commands/)
        script: Option<String>,
        /// Arguments passed to the script
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
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

    // ── Tunnel ─────────────────────────────────────────────────────────────
    /// Share a local port publicly via a reverse SSH tunnel (no extra tools needed)
    Share {
        /// Local port to expose (auto-detected from dip.host labels if omitted)
        #[arg(long, short)]
        port: Option<u16>,

        /// Compose service whose dip.host port to use (ignored when --port is set)
        #[arg(long, short)]
        service: Option<String>,
    },

    // ── System ─────────────────────────────────────────────────────────────
    /// Remove unused Docker resources system-wide (with confirmation)
    Prune {
        /// Also remove unused volumes
        #[arg(long)]
        volumes: bool,
        /// Remove all unused images, not just dangling ones
        #[arg(long, short = 'a')]
        all: bool,
    },
    /// Update dip to the latest version
    Update {
        /// Re-download and reinstall even if already on the latest version
        #[arg(long, short)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum DbCommands {
    /// List detected database services
    List {
        /// Output format: "json" for machine-readable output
        #[arg(long, value_name = "FORMAT")]
        format: Option<String>,
    },
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
    /// Migrate database between MySQL ↔ PostgreSQL (streams rows, no OOM on large DBs)
    Migrate {
        /// Source service name (run `dip db list` to see available services)
        #[arg(long)]
        from: String,
        /// Target service name
        #[arg(long)]
        to: String,
        /// Comma-separated list of tables to migrate (all tables if omitted)
        #[arg(long)]
        tables: Option<String>,
    },
    /// Open an interactive database console (psql / mysql)
    Console {
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
