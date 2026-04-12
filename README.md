# dip

CLI toolkit for Docker Compose projects. Start, stop and inspect containers, manage database dumps, run project scripts, and route local domains via a built-in TLS reverse proxy and DNS resolver.

## Features

- **Project scaffolding** — `dip init` with 14 ready-to-use templates across Node.js, Python, PHP, Ruby, Go and Rust
- **Container lifecycle** — start, stop, restart, reset with full hook lifecycle
- **Database tools** — dump, import, and bidirectional MySQL ↔ PostgreSQL migration (streaming, no OOM on large DBs)
- **Built-in TLS proxy** — reverse proxy with automatic HTTPS, route discovery via `dip.host` labels
- **Built-in DNS server** — no dnsmasq, no external tools — resolves `*.test` out of the box
- **Auto-sync routes** — watches Docker events, updates routes within ~400ms when containers start/stop
- **Public sharing** — `dip share` exposes a local port via a reverse SSH tunnel (no cloudflared, no extra binaries)
- **Desktop notifications** — macOS/Linux notification after long operations (`dip build`, `dip pull`, `dip db migrate`)
- **Project discovery** — `dip ls` lists all dip projects on the machine with live Docker status
- **Certificate info** — `dip cert` shows CA and server cert validity, SANs, keychain trust
- **Custom commands** — shell scripts in `.dip/commands/`, run with `dip run <name>` (lists available scripts when called without arguments)
- **Shell completions** — bash, zsh, fish
- **Self-update** — `dip update` / `dip update --force`

## Installation

```bash
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash
```

Install a specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash -s -- --version 0.1.3
```

Uninstall:

```bash
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash -s -- --uninstall
```

## Quick start

```bash
# First time only — set up TLS proxy and DNS (asks for sudo once)
dip proxy init

# Scaffold a new project
mkdir my-api && cd my-api
dip init                        # shared base only
dip init --template nestjs      # NestJS + PostgreSQL + Valkey

# Start working
dip start
# → containers up, proxy synced, https://my-api.test ready
```

## Project structure

`dip init` creates the following layout:

```
my-project/
├── Dockerfile               ← present when using a framework template
└── .dip/
    ├── default.env          ← commit this — shared defaults
    ├── .env                 ← gitignored — local overrides
    ├── docker-compose.yml
    ├── hooks/
    │   ├── pre-start        ← runs before containers start, stdout → env vars
    │   ├── post-start       ← runs after containers are up
    │   ├── pre-stop         ← runs before containers stop
    │   └── post-stop        ← runs after containers stop
    └── commands/
        ├── utils/color.sh
        └── hello            ← dip run hello
```

### Templates

Every `dip init` always applies the **shared base**: hooks, utility scripts, and an example command. A named template layers its own `docker-compose.yml`, `Dockerfile`, and `commands/` on top.

#### Node.js

| Template    | Stack                                  |
| ----------- | -------------------------------------- |
| `nestjs`    | NestJS · PostgreSQL · Valkey · TypeORM |
| `nextjs`    | Next.js · PostgreSQL · Valkey · Prisma |
| `nuxt`      | Nuxt 3 · PostgreSQL · Valkey · Prisma  |
| `sveltekit` | SvelteKit · PostgreSQL · Prisma        |
| `express`   | Express 5 · PostgreSQL                 |
| `node`      | Node.js bare (bring your own stack)    |

#### Python

| Template  | Stack                                        |
| --------- | -------------------------------------------- |
| `django`  | Django · PostgreSQL · Valkey · Celery        |
| `fastapi` | FastAPI · PostgreSQL · Valkey · Alembic · uv |

#### PHP

| Template    | Stack                                           |
| ----------- | ----------------------------------------------- |
| `laravel`   | Laravel · MySQL · Valkey · nginx · queue worker |
| `wordpress` | WordPress · MySQL · nginx · WP-CLI              |
| `drupal`    | Drupal 11 · MySQL · nginx · Drush               |

#### Other

| Template | Stack                                         |
| -------- | --------------------------------------------- |
| `rails`  | Ruby on Rails · PostgreSQL · Valkey · Sidekiq |
| `go`     | Go · PostgreSQL · Air (hot-reload)            |
| `rust`   | Axum · PostgreSQL · sqlx · cargo-watch        |

```bash
dip init                          # shared base only
dip init --template nestjs
dip init --template django
dip init --template wordpress
# … etc
```

All framework templates:

- include a `Dockerfile` with auto-scaffold on first boot (no manual setup needed — just `dip start`)
- include template-specific `commands/` (e.g. `dip run migrate`)
- use Valkey instead of Redis (drop-in compatible, actively maintained fork)

## Docker Compose labels

### Reverse proxy routing

```yaml
labels:
  dip.host: "${DOMAIN}:80"
  dip.host.api: "api.${DOMAIN}:3000" # multiple hosts per service
```

### Database detection

```yaml
labels:
  dip.db: mysql # or: postgres
```

Credentials are read directly from the container environment — no extra config needed.

## Container commands

```bash
dip start
dip stop
dip restart
dip reset                # stop, remove containers, start fresh
dip build                # build / rebuild service images
dip pull                 # pull latest images
dip remove               # remove containers
dip cleanup              # remove stopped containers and dangling images

dip status               # show container status
dip status --format json # machine-readable output
dip ps                   # alias for status
dip logs                 # stream logs
dip logs db              # logs for a specific service
dip stats                # CPU / memory / I/O
dip top                  # running processes inside containers
dip health               # run health checks on all services

dip shell app            # interactive shell in a container
dip exec app "command"   # run a command inside a container
```

## Custom commands

Place scripts in `.dip/commands/` and run them with `dip run <name>`.

```bash
dip run          # list all available commands with descriptions
dip run migrate
```

Add a `# Description:` comment to any script to show it in the list:

```bash
#!/usr/bin/env bash
# Description: Run pending database migrations

dip exec app "php artisan migrate --force"
```

Scripts are auto-chmod'd to executable — no `chmod +x` needed.

## Database commands

```bash
dip db list                              # show detected DB services
dip db list --format json

dip db dump ./backup.sql                 # plain SQL dump
dip db dump ./backup.sql.gz              # gzip-compressed dump

dip db import ./backup.sql
dip db import ./backup.sql.gz

# Multiple DBs in one project — specify service
dip db dump ./backup.sql --service mysql
dip db dump ./analytics.sql --service postgres
```

### Database migration

Migrate between MySQL and PostgreSQL in either direction. Rows are streamed in chunks — memory usage stays constant regardless of table size.

```bash
dip db migrate --from mysql --to postgres
dip db migrate --from postgres --to mysql
dip db migrate --from mysql --to postgres --tables users,orders,products
```

What gets migrated: schema, data (streamed 500 rows at a time), indexes, foreign keys, sequences.

## Reverse proxy

```bash
dip proxy init      # generate CA + cert, set up DNS (sudo once)
dip proxy start
dip proxy stop
dip proxy restart
dip proxy status
dip proxy routes    # list all routing rules
dip proxy logs      # tail access log
dip proxy sync      # manually sync routes from running containers
```

Routes are discovered automatically from `dip.host` labels when you run `dip start`.  
The proxy also watches Docker events and updates routes automatically when containers start or stop.

### Manual route management

```bash
dip proxy add api.myapp.test 127.0.0.1:3000
dip proxy remove api.myapp.test
```

## DNS

dip includes a built-in DNS server — no dnsmasq, no Homebrew required.

`dip proxy init` configures DNS automatically. macOS uses `/etc/resolver/`, Linux uses systemd-resolved.

```bash
dip proxy config                          # show current settings
dip proxy config --tld myapp             # resolve *.myapp instead of *.test
dip proxy config --tld "test,local"      # multiple TLDs
dip proxy config --dns-port 5381         # change DNS port
```

> **Note:** The proxy handles HTTP/HTTPS only (ports 80/443). For TCP services (MySQL, PostgreSQL), expose the port in `docker-compose.yml` and connect via `myapp.test:3306` — DNS resolves all `*.test` to `127.0.0.1`.

## Sharing

`dip share` opens a public HTTPS tunnel to a local port — no cloudflared, no extra tools.

```bash
dip share                    # auto-detect port from dip.host labels
dip share --port 3000
dip share --service backend  # pick port from a specific compose service
```

```
  Tunneling localhost:3000 → localhost.run
  Press Ctrl+C to stop

  ✓ Public URL: https://abc123def.lhr.life
```

## Notifications

Long-running commands send a desktop notification when they finish.

| Command          | Notification                                   |
| ---------------- | ---------------------------------------------- |
| `dip build`      | `my-project — build complete`                  |
| `dip pull`       | `my-project — pull complete`                   |
| `dip db migrate` | `my-project — migrate mysql→postgres complete` |

macOS uses `osascript` (built-in). Linux uses `notify-send` (libnotify).

## Utilities

```bash
dip env                  # show resolved project environment variables
dip ls                   # list all dip projects on this machine
dip ls --root ~/work
dip cert                 # show TLS certificate info
dip open                 # open project URL in browser
dip open api             # open a specific service domain
dip sysinfo              # show system and Docker environment info
dip completions zsh      # generate shell completions
dip update               # update to the latest version
dip update --force       # reinstall even if already on latest
```

Add completions to your shell:

```bash
dip completions zsh
# add to ~/.zshrc:
source ~/.config/dip/completions/dip.zsh
```

## Hooks

Scripts in `.dip/hooks/` run automatically during the container lifecycle.  
Stdout from `pre-start` is parsed as `KEY=VALUE` env vars injected into docker-compose.

| Hook         | Failure | When                    |
| ------------ | ------- | ----------------------- |
| `pre-start`  | aborts  | before containers start |
| `post-start` | warning | after containers are up |
| `pre-stop`   | warning | before containers stop  |
| `post-stop`  | warning | after containers stop   |

```bash
#!/usr/bin/env bash
# .dip/hooks/pre-start — export AWS credentials into the compose environment
aws configure export-credentials --format env
```

## License

[MIT](LICENSE)
