# dip

CLI toolkit for Docker Compose projects. Start, stop and inspect containers, manage database dumps, run project scripts, and route local domains via a built-in TLS reverse proxy and DNS resolver.

## Installation

```bash
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash
```

```bash
# specific version
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash -s -- --version 0.1.3
# uninstall
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash -s -- --uninstall
```

## Quick start

```bash
# First time — set up TLS proxy and DNS (asks for sudo once)
dip proxy init

# Scaffold a new project
mkdir my-api && cd my-api
dip init --template nestjs

# Start working
dip start
# → containers up, proxy synced, https://my-api.test ready
```

## Project structure

```
my-project/
├── Dockerfile
└── .dip/
    ├── default.env          ← commit this
    ├── .env                 ← gitignored
    ├── docker-compose.yml
    ├── hooks/               ← pre-start, post-start, pre-stop, post-stop
    └── commands/            ← dip run <name>
```

## Templates

`dip init` is interactive — pick a template by number or pass `--template <name>`:

| Template    | Stack                                           |
| ----------- | ----------------------------------------------- |
| `nestjs`    | NestJS · PostgreSQL · Valkey · TypeORM          |
| `nextjs`    | Next.js · PostgreSQL · Valkey · Prisma          |
| `nuxt`      | Nuxt 3 · PostgreSQL · Valkey · Prisma           |
| `sveltekit` | SvelteKit · PostgreSQL · Prisma                 |
| `react`     | React · Vite · TypeScript                       |
| `angular`   | Angular CLI · TypeScript                        |
| `express`   | Express 5 · PostgreSQL                          |
| `node`      | Node.js bare                                    |
| `django`    | Django · PostgreSQL · Valkey · Celery           |
| `fastapi`   | FastAPI · PostgreSQL · Valkey · Alembic · uv    |
| `laravel`   | Laravel · MySQL · Valkey · nginx · queue worker |
| `wordpress` | WordPress · MySQL · nginx · WP-CLI              |
| `drupal`    | Drupal 11 · MySQL · nginx · Drush               |
| `rails`     | Ruby on Rails · PostgreSQL · Valkey · Sidekiq   |
| `go`        | Go · PostgreSQL · Air (hot-reload)              |
| `rust`      | Axum · PostgreSQL · sqlx · cargo-watch          |

Every template includes a `Dockerfile` with auto-scaffold on first boot and template-specific `commands/` (e.g. `dip run migrate`).

## Docker Compose labels

```yaml
labels:
  dip.host: "${DOMAIN}:80" # proxy routing
  dip.host.api: "api.${DOMAIN}:3000" # multiple hosts per service
  dip.db: mysql # database detection (or: postgres)
```

## Commands

### Container lifecycle

```bash
dip start [service]      # start containers  (alias: up)
dip stop [service]       # stop containers   (alias: down)
dip restart [service]    # restart           (alias: reup)
dip build [service]      # build images
dip pull                 # pull latest images
dip reset                # stop, remove, start fresh
dip remove [service]     # remove containers
dip cleanup              # remove stopped containers and dangling images
```

### Status and logs

```bash
dip status               # container status table
dip status --watch       # auto-refresh every 2s
dip status --watch --interval 5
dip status --format json
dip ps                   # alias for status

dip logs                 # stream all logs (tail 100)
dip logs app             # specific service
dip logs --since 1h      # show logs from the last hour
dip logs --since 30m
dip logs --errors        # only error/exception/fatal/panic lines
dip logs --warn          # only warning lines
dip logs --sql           # only SQL queries
dip logs --http          # only HTTP requests + status codes
dip logs --slow          # only slow/timeout/deadline lines
dip logs --grep "userId" # filter by pattern (case-insensitive)
dip logs app --errors --grep "auth"   # flags compose freely

dip stats [service]      # CPU / memory / I/O
dip top [service]        # running processes inside containers
dip health               # run health checks
dip doctor               # check Docker, proxy, certs, DNS, ports, Linux caps
```

### Shell and exec

```bash
dip shell app            # interactive shell in a container
dip exec app "command"   # run a one-off command
```

### Custom commands

Scripts in `.dip/commands/` run with `dip run <name>`. Add a `# Description:` line to show it in the list:

```bash
#!/usr/bin/env bash
# Description: Run pending database migrations
dip exec app "php artisan migrate --force"
```

```bash
dip run              # list all available commands
dip run migrate
```

### Database

```bash
dip db list                                          # show detected DB services
dip db console [--service mysql]                     # interactive psql / mysql shell
dip db dump ./backup.sql [--service mysql]           # plain SQL dump
dip db dump ./backup.sql.gz                          # gzip-compressed
dip db import ./backup.sql
dip db migrate --from mysql --to postgres            # bidirectional migration
dip db migrate --from mysql --to postgres --tables users,orders
```

Migration streams rows in chunks of 500 — memory stays constant regardless of table size. Migrates schema, data, indexes, foreign keys, and sequences.

### Reverse proxy

```bash
dip proxy init            # one-time setup: CA cert, DNS, keychain trust
dip proxy start / stop / restart / status
dip proxy logs [-n 100]   # tail access log
dip proxy routes          # list routing rules
dip proxy sync            # manually sync routes from running containers
dip proxy add api.myapp.test 127.0.0.1:3000
dip proxy remove api.myapp.test
dip proxy config                          # show current settings
dip proxy config --tld myapp             # resolve *.myapp instead of *.test
dip proxy config --tld "test,local"      # multiple TLDs
dip proxy config --dns-port 5381
```

Routes are discovered automatically from `dip.host` labels on `dip start` and updated in ~400ms when containers start or stop.

### DNS

dip includes a built-in DNS server — no dnsmasq, no Homebrew.

`dip proxy init` configures DNS interactively (TLD, port, upstream servers).

- **macOS** — writes `/etc/resolver/<tld>` (custom port supported, no extra setup)
- **Linux** — configures systemd-resolved; defaults to port 53 so no iptables redirect is needed; uses `setcap cap_net_bind_service` once

> The proxy handles HTTP/HTTPS (ports 80/443). TCP services like MySQL/PostgreSQL work via `myapp.test:3306` — DNS resolves all `*.test` to `127.0.0.1`.

### Sharing

```bash
dip share                   # expose port from dip.host labels
dip share --port 3000
dip share --service backend
```

Opens a public HTTPS tunnel via a reverse SSH tunnel — no cloudflared, no extra binaries.

### Utilities

```bash
dip env               # show resolved project environment variables
dip ls [--root ~/work] # list all dip projects on this machine with live status
dip cert              # TLS certificate info (validity, SANs, keychain trust)
dip open [service]    # open project URL in browser
dip sysinfo           # system and Docker environment info
dip prune [--volumes] [--all]
dip update [--force]
dip completions zsh   # generate shell completions (bash / zsh / fish)
```

## Shell integration

Run once to enable completions and project commands:

```bash
dip completions zsh   # generates file and offers to add to ~/.zshrc
```

After sourcing: tab completions activate for all `dip` subcommands, and scripts from `.dip/commands/` become available as plain shell commands from any subdirectory of the project:

```bash
cd my-laravel-app
migrate        # → dip run migrate
queue-work     # → dip run queue-work
```

## Hooks

Scripts in `.dip/hooks/` run automatically during the container lifecycle. Stdout from `pre-start` is parsed as `KEY=VALUE` and injected into docker-compose.

| Hook         | On failure | When                    |
| ------------ | ---------- | ----------------------- |
| `pre-start`  | aborts     | before containers start |
| `post-start` | warning    | after containers are up |
| `pre-stop`   | warning    | before containers stop  |
| `post-stop`  | warning    | after containers stop   |

```bash
#!/usr/bin/env bash
# .dip/hooks/pre-start — export AWS credentials into the compose environment
aws configure export-credentials --format env
```

## Notifications

Desktop notification when long-running commands finish (`dip build`, `dip pull`, `dip db migrate`). Uses `osascript` on macOS and `notify-send` on Linux.

## License

[MIT](LICENSE)
