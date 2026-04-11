# dip

Docker Integration Platform — a fast CLI tool for managing Docker Compose projects.

```
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash
```

## Features

- **Project scaffolding** — `dip init` generates docker-compose, env files, hooks and commands
- **Container lifecycle** — start, stop, restart, reset with pre-start hooks
- **Database tools** — dump and import for MySQL and PostgreSQL, auto-detected via `dip.db` labels
- **Built-in TLS proxy** — reverse proxy with automatic HTTPS and route discovery via `dip.host` labels
- **Custom commands** — shell scripts in `.dip/commands/`, run with `dip run <name>`
- **Self-update** — `dip update` downloads the latest release for your platform

## Installation

```bash
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash
```

Install a specific version:

```bash
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash -s -- --version 1.0.0
```

Uninstall:

```bash
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash -s -- --uninstall
```

## Quick start

```bash
mkdir my-project && cd my-project
dip init
# edit .dip/docker-compose.yml
dip start
```

## Project structure

`dip init` creates the following layout:

```
.dip/
├── default.env          # committed — shared defaults
├── .env                 # gitignored — local overrides
├── docker-compose.yml
├── hooks/
│   └── pre-start        # runs before containers start, stdout parsed as env vars
└── commands/
    ├── utils/color.sh
    └── hello            # dip run hello
```

## Docker Compose labels

### Reverse proxy

```yaml
labels:
  dip.host: "${DOMAIN}:80"
  dip.host.api: "api.${DOMAIN}:3000"   # multiple hosts per service
```

### Database

```yaml
labels:
  dip.db: mysql     # or postgres
```

Credentials are read directly from the container environment — no `.env` needed.

## Database commands

```bash
# Single DB — no flags needed
dip db dump ./backup.sql
dip db import ./backup.sql

# Multiple DBs — specify service
dip db dump ./backup.sql --service mysql
dip db dump ./analytics.sql --service postgres
```

## Reverse proxy

```bash
dip proxy init     # generate CA + cert, set up DNS (optional)
dip proxy status
dip proxy routes
dip proxy logs
```

Routes are discovered automatically from `dip.host` labels when you run `dip start`.

### DNS setup

If you don't use Pi-hole or another DNS server, `dip proxy init` can configure dnsmasq to resolve `*.test` to `127.0.0.1` automatically.

## Pre-start hooks

The `.dip/hooks/pre-start` script runs before containers start. Its **stdout** is parsed as `KEY=VALUE` env vars and injected into docker-compose.

```bash
#!/usr/bin/env bash
# Export AWS credentials into the compose environment
aws configure export-credentials --format env
```

## Custom commands

Place executable scripts in `.dip/commands/` and run them with `dip run <name>`.

```bash
#!/usr/bin/env bash
# .dip/commands/composer
source "${DIP_DIR}/commands/utils/color.sh"

msg "${YELLOW}[INFO]${NOFORMAT} Running composer $*"
dip exec backend "cd /var/www && composer $*"
```

```bash
dip run composer install
dip run composer require vendor/package
```

## License

[MIT](LICENSE)
