# dip

Docker Integration Platform — a fast CLI tool for managing Docker Compose projects.

```
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash
```

## Features

- **Project scaffolding** — `dip init` generates docker-compose, env files, hooks and commands
- **Container lifecycle** — start, stop, restart, reset with full hook lifecycle
- **Database tools** — dump, import and list for MySQL and PostgreSQL, auto-detected via `dip.db` labels
- **Built-in TLS proxy** — reverse proxy with automatic HTTPS and route discovery via `dip.host` labels
- **DNS setup** — optional automatic dnsmasq configuration for `*.test` domains
- **Custom commands** — shell scripts in `.dip/commands/`, run with `dip run <name>`
- **Shell completions** — bash, zsh, fish
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
# First time only — set up TLS proxy and DNS
dip proxy init

# Then for each new project
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
│   ├── pre-start        # runs before containers start
│   ├── post-start       # runs after containers are up
│   ├── pre-stop         # runs before containers stop
│   └── post-stop        # runs after containers stop
└── commands/
    ├── utils/color.sh
    └── hello            # dip run hello
```

## Docker Compose labels

### Reverse proxy

```yaml
labels:
  dip.host: "${DOMAIN}:80"
  dip.host.api: "api.${DOMAIN}:3000" # multiple hosts per service
```

### Database

```yaml
labels:
  dip.db: mysql # or postgres
```

Credentials are read directly from the container environment — no `.env` needed.

## Database commands

```bash
dip db list                              # show detected DB services

# Single DB — no flags needed
dip db dump ./backup.sql
dip db import ./backup.sql

# Multiple DBs — specify service
dip db dump ./backup.sql --service mysql
dip db dump ./analytics.sql --service postgres
```

## Reverse proxy

```bash
dip proxy init     # generate CA + cert, optionally set up DNS
dip proxy status
dip proxy routes
dip proxy logs
```

Routes are discovered automatically from `dip.host` labels when you run `dip start`.

### DNS setup

`dip proxy init` will ask if you want to configure DNS automatically. If you use Pi-hole or another DNS server, skip this step.

On macOS, dip installs dnsmasq via Homebrew and creates `/etc/resolver/test`.
On Linux, dip configures dnsmasq and systemd-resolved.

## Hooks

Scripts in `.dip/hooks/` run automatically during the container lifecycle. Stdout from hooks is parsed as `KEY=VALUE` env vars and injected into docker-compose.

| Hook         | When                    |
| ------------ | ----------------------- |
| `pre-start`  | before containers start |
| `post-start` | after containers are up |
| `pre-stop`   | before containers stop  |
| `post-stop`  | after containers stop   |

```bash
#!/usr/bin/env bash
# .dip/hooks/pre-start — export AWS credentials into the compose environment
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

## Utilities

```bash
dip env                  # show resolved project environment variables
dip completions zsh      # generate shell completions
```

Add completions to your shell:

```bash
dip completions zsh
# then add to ~/.zshrc:
source ~/.config/dip/completions/dip.zsh
```

## License

[MIT](LICENSE)
