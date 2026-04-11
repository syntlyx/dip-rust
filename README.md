# dip

Docker Integration Platform — a fast CLI tool for managing Docker Compose projects.

```bash
curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash
```

## Features

- **Project scaffolding** — `dip init` with templates for NestJS, Next.js, Node, Laravel
- **Container lifecycle** — start, stop, restart, reset with full hook lifecycle
- **Database tools** — dump and import for MySQL and PostgreSQL (with gzip support), auto-detected via `dip.db` labels
- **Built-in TLS proxy** — reverse proxy with automatic HTTPS, route discovery via `dip.host` labels
- **Built-in DNS server** — no dnsmasq, no external tools — resolves `*.test` out of the box
- **Auto-sync routes** — watches Docker events, updates routes within ~400ms when containers start/stop
- **Project discovery** — `dip ls` lists all dip projects on the machine with live Docker status
- **Certificate info** — `dip cert` shows CA and server cert validity, SANs, keychain trust
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
# First time only — set up TLS proxy and DNS (asks for sudo once)
dip proxy init

# Scaffold a new project
mkdir my-project && cd my-project
dip init                        # bare project
dip init --template nestjs      # NestJS + PostgreSQL + Redis

# Start working
dip start
# → containers up, proxy synced, https://my-project.test ready
```

## Project structure

`dip init` creates the following layout:

```
.dip/
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

```bash
dip init --template nestjs    # NestJS + PostgreSQL + Redis
dip init --template nextjs    # Next.js + PostgreSQL
dip init --template node      # Node.js
dip init --template laravel   # Laravel + MySQL + Redis
```

Each template generates a `Dockerfile` alongside `.dip/`.

## Docker Compose labels

### Reverse proxy routing

```yaml
labels:
  dip.host: "${DOMAIN}:80"
  dip.host.api: "api.${DOMAIN}:3000"  # multiple hosts per service
```

### Database detection

```yaml
labels:
  dip.db: mysql   # or: postgres
```

Credentials are read directly from the container environment — no extra config needed.

## Database commands

```bash
dip db list                              # show detected DB services

dip db dump ./backup.sql                 # plain SQL dump
dip db dump ./backup.sql.gz              # gzip-compressed dump

dip db import ./backup.sql
dip db import ./backup.sql.gz

# Multiple DBs in one project — specify service
dip db dump ./backup.sql --service mysql
dip db dump ./analytics.sql --service postgres
```

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
The proxy also watches Docker events and updates routes automatically when containers start or stop — no manual sync needed after OrbStack restarts or image rebuilds.

### Manual route management

```bash
dip proxy add api.myapp.test 127.0.0.1:3000
dip proxy remove api.myapp.test
```

## DNS

dip includes a built-in DNS server — no dnsmasq, no Homebrew required.

`dip proxy init` asks if you want to configure DNS automatically. If you use Pi-hole or another DNS server, skip this step.

**macOS:** takes ownership of `/etc/resolver/` (sudo once), then writes `/etc/resolver/test`.  
**Linux:** configures systemd-resolved.

### Changing DNS settings

```bash
dip proxy config                          # show current settings
dip proxy config --tld myapp             # resolve *.myapp instead of *.test
dip proxy config --tld "test,local"      # multiple TLDs
dip proxy config --dns-port 5381         # change DNS port
```

After `dip proxy init`, all DNS changes are applied without sudo.

## Certificates

```bash
dip cert
```

Shows CA and server certificate info:

```
certificates
  CA certificate
    Path:      ~/.dip/ca.pem
    Valid:     2024-01-01 → 2034-01-01  (expires in 3284 days)
    Keychain:  installed in system keychain ✓

  Server certificate
    Path:      ~/.dip/server.pem
    Valid:     2024-01-01 → 2025-01-01  (expires in 264 days)
    SANs:      *.myapp.test, myapp.test
```

> **Note:** The proxy handles HTTP/HTTPS traffic only (ports 80/443). For TCP services like MySQL or PostgreSQL, expose the port in `docker-compose.yml` and connect using the DNS name with an explicit port — e.g. `myapp.test:3306` in Sequel Ace. DNS resolves all `*.test` hostnames to `127.0.0.1`.

## Project discovery

```bash
dip ls              # scan home directory for dip projects
dip ls --root ~/work
```

Lists all dip projects found on the machine with live Docker status:

```
dip projects  (3 found, 1 running)
  ●  my-api               ~/work/my-api                     2 containers
  ○  blog                 ~/work/blog                       stopped
  ○  old-project          ~/Projects/old-project            stopped
```

## Hooks

Scripts in `.dip/hooks/` run automatically during the container lifecycle.  
Stdout from `pre-start` is parsed as `KEY=VALUE` env vars injected into docker-compose.

| Hook         | Failure  | When                    |
|--------------|----------|-------------------------|
| `pre-start`  | aborts   | before containers start |
| `post-start` | warning  | after containers are up |
| `pre-stop`   | warning  | before containers stop  |
| `post-stop`  | warning  | after containers stop   |

```bash
#!/usr/bin/env bash
# .dip/hooks/pre-start — export AWS credentials into the compose environment
aws configure export-credentials --format env
```

Hook scripts are auto-chmod'd to executable — no manual `chmod +x` needed.

## Custom commands

Place scripts in `.dip/commands/` and run them with `dip run <name>`.

```bash
#!/usr/bin/env bash
# .dip/commands/composer
source "${DIP_DIR}/commands/utils/color.sh"

msg "${YELLOW}Running composer $*${NOFORMAT}"
docker compose exec backend composer "$@"
```

```bash
dip run composer install
dip run composer require vendor/package
```

## Utilities

```bash
dip env                  # show resolved project environment variables
dip ls                   # list all dip projects on this machine
dip cert                 # show TLS certificate info
dip open                 # open project URL in browser
dip open api             # open a specific service domain
dip sysinfo              # show system and Docker environment info
dip completions zsh      # generate shell completions
```

Add completions to your shell:

```bash
dip completions zsh
# add to ~/.zshrc:
source ~/.config/dip/completions/dip.zsh
```

## License

[MIT](LICENSE)
