# dip

`dip` is a local development CLI for containerized projects. It gives each
project a small `.dip/` workspace with Compose config, environment files, hooks,
project scripts, database helpers, and local HTTPS routing.

It is built around Docker-compatible runtimes by default: Docker Desktop,
OrbStack, Colima, or any setup that exposes `docker compose`. On macOS, `dip`
can also run an experimental Apple Container provider.

## What It Does

- Scaffolds practical starter projects with `.dip/` config.
- Starts, stops, restarts, builds, and inspects project services.
- Routes local domains like `https://my-app.test` through a built-in TLS proxy.
- Runs project scripts from `.dip/commands/`.
- Dumps, imports, consoles into, and migrates MySQL/PostgreSQL databases.
- Supports runtime-aware workflows for Docker-compatible engines and Apple Container.
- Benchmarks runtime startup, exec latency, and disk I/O.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash
```

Install a specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash -s -- --version 0.1.8
```

Update or uninstall:

```bash
dip update
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash -s -- --uninstall
```

Release binaries are published for macOS arm64/x64 and Linux arm64/x64 musl.

## Quick Start

```bash
# One-time local HTTPS + DNS setup.
dip proxy init

# Create a project from a template.
mkdir my-api
cd my-api
dip init --template nestjs

# Start the stack.
dip start

# Open the routed local URL.
dip open
```

After `dip start`, proxy routes are synced from `dip.host` labels in
`.dip/docker-compose.yml`. A template project is usually reachable at
`https://<project>.test`.

## Project Layout

```text
my-project/
|-- .dip/
|   |-- default.env          # committed defaults
|   |-- .env                 # local overrides, gitignored
|   |-- docker-compose.yml
|   |-- hooks/
|   |   |-- pre-start
|   |   `-- post-start
|   `-- commands/
|       `-- migrate
`-- Dockerfile
```

`default.env` is the shared contract. `.dip/.env` is the local copy. Hooks and
commands are plain executable scripts.

## Templates

`dip init` is interactive, or you can pass `--template <name>`.

| Template     | Stack                                       |
| ------------ | ------------------------------------------- |
| `nestjs`     | NestJS, PostgreSQL, Valkey, TypeORM         |
| `nextjs`     | Next.js, PostgreSQL, Valkey, Prisma         |
| `nuxt`       | Nuxt 3, PostgreSQL, Valkey, Prisma          |
| `sveltekit`  | SvelteKit, PostgreSQL, Prisma               |
| `react`      | React, Vite, TypeScript                     |
| `angular`    | Angular CLI, TypeScript                     |
| `express`    | Express 5, PostgreSQL                       |
| `node`       | Node.js bare                                |
| `node-multi` | Node.js multi-service workspace, PostgreSQL |
| `django`     | Django, PostgreSQL, Valkey, Celery          |
| `fastapi`    | FastAPI, PostgreSQL, Valkey, Alembic, uv    |
| `laravel`    | Laravel, MySQL, Valkey, nginx, queue worker |
| `wordpress`  | WordPress, MySQL, nginx, WP-CLI             |
| `drupal`     | Drupal 11, MySQL, nginx, Drush              |
| `rails`      | Ruby on Rails, PostgreSQL, Valkey, Sidekiq  |
| `go`         | Go, PostgreSQL, Air                         |
| `rust`       | Axum, PostgreSQL, sqlx, cargo-watch         |

Every template layers shared `.dip` defaults with stack-specific Compose,
Dockerfile, hooks, and commands.

## Daily Commands

Lifecycle:

```bash
dip start [service]      # alias: dip up
dip stop [service]       # alias: dip down
dip restart [service]    # alias: dip reup
dip build [service]
dip pull
dip reset
dip remove [service]
dip cleanup
```

Status and logs:

```bash
dip status
dip status --watch --interval 5
dip status --format json
dip ps

dip logs
dip logs app
dip logs --since 1h
dip logs --errors
dip logs --warn
dip logs --sql
dip logs --http
dip logs --slow
dip logs --grep "auth"

dip stats [service]
dip top [service]
dip health
```

Shell and exec:

```bash
dip shell app
dip shell app --type sh
dip bash app
dip exec app pnpm test

# Arguments are passed straight to the container (no extra shell layer),
# so quotes and shell metacharacters like () * ; survive intact:
dip exec db psql -U postgres -c "SELECT count(*) FROM users WHERE id IN (1,2,3)"

# For pipes, redirects, or variable expansion, invoke a shell explicitly:
dip exec app sh -c "ls | wc -l"
```

Project scripts:

```bash
dip run              # list .dip/commands
dip run migrate
dip run pnpm install
```

Add a description to a command by placing this near the top of the script:

```bash
# Description: Run pending database migrations
```

## Environment And Config Checks

```bash
dip env
dip env diff
dip env diff --show-values

dip validate
dip validate --fix
dip explain build
dip explain build app
dip doctor
dip sysinfo
```

`validate --fix` only applies narrow, safe fixes for known `.dip` config drift.

## Local HTTPS Proxy

`dip` includes a built-in reverse proxy and DNS server. It is intended for local
development domains such as `*.test`.

```bash
dip proxy init
dip proxy start
dip proxy stop
dip proxy restart
dip proxy status
dip proxy logs -n 100
dip proxy routes
dip proxy sync
```

Manual routes:

```bash
dip proxy add api.my-app.test 127.0.0.1:3000
dip proxy remove api.my-app.test
```

DNS config:

```bash
dip proxy config
dip proxy config --tld myapp
dip proxy config --tld "test,local"
dip proxy config --dns-port 5381
```

Platform setup:

- macOS: writes `/etc/resolver/<tld>` and installs the local CA in the system keychain.
- Linux: configures systemd-resolved and can bind DNS on port 53 with `cap_net_bind_service`.

Docker-compatible runtimes can also use the proxy watcher, which listens to
Docker container events and refreshes routes as containers start or stop. Apple
Container does not expose Docker events, so Apple routes are synced by `dip`
commands such as `start`, `restart`, and `proxy sync`.

## Compose Labels

Add labels to services in `.dip/docker-compose.yml`:

```yaml
services:
  app:
    labels:
      dip.host: "${DOMAIN}:3000"
      dip.host.api: "api.${DOMAIN}:3000"
      dip.db: postgres
```

`dip.host` labels create proxy routes. `dip.db` lets `dip db` discover database
credentials from the running container environment. Supported database values are
`postgres` and `mysql`.

## Database Commands

```bash
dip db list
dip db console
dip db console --service db

dip db dump ./backup.sql
dip db dump ./backup.sql.gz
dip db import ./backup.sql

dip db migrate --from mysql --to postgres
dip db migrate --from mysql --to postgres --tables users,orders
```

Migration streams rows in chunks, so memory use stays bounded. It handles schema,
data, indexes, foreign keys, and sequences for common MySQL/PostgreSQL projects.

## Runtime Support

### Docker-Compatible Runtimes

This is the default on macOS and Linux. Anything that provides `docker` and
`docker compose` should work:

- Docker Desktop
- OrbStack
- Colima
- native Docker Engine on Linux

On Linux, runtime selection is disabled. `dip` always uses the Docker-compatible
backend.

### Apple Container

Apple Container support is macOS-only and experimental. It is useful if you want
to try Apple's `container` runtime without a Docker subscription.

Run one command with Apple Container:

```bash
DIP_RUNTIME=apple dip start
```

Set it globally on macOS:

```bash
dip use apple
dip use docker
dip use auto
```

Pin only the current project:

```bash
dip use apple --project
dip use auto --project
```

Runtime precedence on macOS:

1. `DIP_RUNTIME`
2. `.dip/runtime`
3. `.dip/.env`
4. `~/.config/dip/runtime`
5. Docker-compatible backend

The Apple provider reads `.dip/docker-compose.yml` directly and translates a
practical Compose subset to the `container` CLI: images, builds, env files,
environment, labels, bind/named volumes, ports, basic dependencies, logs, exec,
status, healthchecks, DB dump/import, DB migration discovery, and proxy route
sync.

Some advanced Compose features may still require a Docker-compatible runtime.

## Benchmarks

`dip bench` compares Apple Container with the Docker-compatible runtime on macOS.
On Linux it benchmarks the single Docker-compatible runtime.

Cold lifecycle benchmark:

```bash
dip bench runtime --iterations 10 --warmup 2 --size-mb 256
```

Project bind-mount I/O:

```bash
dip bench project-io --iterations 10 --warmup 2 --size-mb 256
```

Steady-state benchmark inside one already-running test container:

```bash
dip bench steady --iterations 100 --warmup 5 --size-mb 256
dip bench steady --project-io --iterations 100 --warmup 5 --size-mb 256
```

Useful custom options:

```bash
dip bench runtime --image alpine:latest --path /tmp/dip-bench.bin --json
```

## Shell Integration

Generate shell completions:

```bash
dip completions zsh
```

After sourcing completions, scripts from `.dip/commands/` can also be exposed as
plain shell commands from inside the project tree.

## Hooks

Hooks live in `.dip/hooks/` and run around lifecycle commands:

| Hook         | Failure behavior | Runs                    |
| ------------ | ---------------- | ----------------------- |
| `pre-start`  | aborts           | before containers start |
| `post-start` | warning          | after containers start  |
| `pre-stop`   | warning          | before containers stop  |
| `post-stop`  | warning          | after containers stop   |

`pre-start` may print `KEY=VALUE` lines; those values are injected into the
project runtime environment.

Example:

```bash
#!/usr/bin/env bash
# .dip/hooks/pre-start
aws configure export-credentials --format env
```

## Sharing

```bash
dip share
dip share --port 3000
dip share --service backend
```

This opens a public HTTPS tunnel via a reverse SSH tunnel. No cloudflared or
extra tunnel binary is required.

## Other Utilities

```bash
dip ls [--root ~/work]
dip open [service]
dip cert
dip prune [--volumes] [--all]
dip update [--force]
dip completions zsh
```

## License

[MIT](LICENSE)
