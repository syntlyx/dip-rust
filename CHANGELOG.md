# Changelog

## 0.2.1 — 2026-11-09

Performance and correctness on the exec path: `dip exec` now talks to the
Docker Engine API directly, and several long-standing pass-through bugs
around it — swallowed exit codes, chatter on stdout — are fixed.

### Changed

- **The daemon check and `dip ls` stopped shelling out.** `check_daemon()`
  ran `docker info` (~32 ms) purely to learn whether the daemon answers;
  `GET /_ping` answers the same question in ~0.4 ms, and it runs ahead of
  six `dip db` subcommands plus `remove`, `prune` and `sysinfo`. `dip ls`
  ran one `docker ps --filter` per discovered project (~11 ms each, in a
  thread apiece); one `/containers/json` sweep (~0.5 ms) now returns every
  running container and the per-project tally is done here — so the cost no
  longer grows with the number of projects. `dip ls` produces correct
  counts with no `docker` binary on `PATH` at all.

  `dip db list` also spawned the compose plugin directly for its `ps -q`,
  bypassing the fast path every other `ps` already used. Routed through it,
  the command drops from ~135 ms to ~30 ms; the remaining `docker inspect`
  (~22 ms) stays, because it reads container environment that the list
  endpoint does not carry.

  Every one of these falls back to the CLI when the API is unreachable.

- **`dip exec` runs against the Docker Engine API directly** — no compose
  plugin, and no docker CLI either. The CLI's own startup dominates a
  short exec: `docker exec <id> true` measures ~23 ms against a live
  daemon, of which only ~13 ms is the daemon running the command. Doing
  create/start/inspect ourselves costs ~14 ms, so dip lands below the
  CLI it used to wrap. Measured on macOS + Docker 29.8, 60 interleaved
  samples on a loaded desktop (minimum, the cleanest signal — medians
  ran 5-7 ms higher across the board):

  | | min | median |
  |---|---|---|
  | `dip exec app true` | **19.1 ms** | **26.1 ms** |
  | `docker exec <id> true` | 22.7 ms | 27.5 ms |
  | `docker compose exec -i app true` | 36.7 ms | 39.4 ms |

  Three tiers: the Engine API, then `docker exec` addressing the container
  by ID, then the compose plugin, which stays the source of truth for
  compose semantics. A scaled or stopped service, a compose-only flag
  (`--index`, `-d`, `-T`) or an unreachable API falls back to the next
  tier.

  Falling back stops the moment the daemon has been told to start the
  command: from that point a failure is reported rather than retried,
  because retrying would run the command *twice*. Losing the connection
  while reading the output of `php artisan migrate` must not run the
  migration again. Relatedly, `dip exec app … | head` closing our stdout
  early is an ordinary end to a pipeline, not a failure — dip stops and
  exits 141, exactly as `docker exec … | head` does.

  **Anything wanting a pseudo-TTY takes tier 2** — `dip shell`, `dip bash`,
  and `dip exec` run straight from a terminal. A TTY exec also needs raw
  terminal mode and `SIGWINCH` forwarding, which the API path does not do;
  it still skips the compose plugin, so ~26 ms rather than ~39 ms. The
  full API speedup applies where it compounds: `dip run` scripts, CI, and
  anything piped or redirected.

  The tiers carry the same environment compose would have — its `exec`
  forwards only the command-line `-e`/`--workdir`/`--user` flags to the
  Engine API and never merges the service's compose-file `environment:`
  or `working_dir`, which are already baked into the container. The
  *flags* differ, though, and this is the one place the translation is
  not literal: `compose exec` allocates a pseudo-TTY unless given `-T`,
  while `docker exec` and the API allocate one only when asked, so a bare
  `-i` means "TTY" to compose and "no TTY" to them. dip now asks for `-t`
  explicitly whenever both stdin and stdout are terminals — without it,
  every tool that colorizes on `isatty(stdout)` (phpunit, phpcs,
  composer, npm) silently loses its colours.

  Requesting the TTY only when it can actually work also drops a compose
  failure mode: compose refuses a piped stdin with a TTY-enabled
  container ("cannot attach stdin to a TTY-enabled container"), so
  `echo "select 1" | dip exec db psql` failed from a terminal. It runs
  now.

  Verified byte-for-byte against `docker exec` for a 5 MB stdin
  round-trip and a 5 MB stdout stream, and across all four
  stdin/stdout terminal-vs-pipe combinations.

### Fixed

- **`dip db import` fed archives to the database client as if they were SQL.**
  The format came from the file name — anything ending in `.gz` went
  through `gunzip -c`, everything else straight to `psql`/`mysql`. So
  `backup.tar.gz` matched the `.gz` rule, decompressed to raw tar bytes,
  and handed *those* to the client: a wall of syntax errors partway
  through, and a half-applied dump rather than a refusal. The format is
  now read from the file's leading bytes, gzip included — its contents
  are inflated and sniffed too, which is the only way to tell a gzipped
  SQL dump from a gzipped tarball. A gzipped dump named `.sql` therefore
  imports correctly as well.

  What it cannot stream is now named and rejected before anything reaches
  the database, with the command that unpacks it: `tar`, `tar.gz` and
  `zip` archives, `zstd`/`bzip2`/`xz`/`lz4` compression, `pg_dump`'s
  custom format (pointing at `pg_restore`), truncated gzip streams, empty
  files, and binary data that is no recognized format at all. `dip db
  dump` still picks compression from the output name, which is the only
  signal available before the file exists — and the one the user chose.

- **dip coloured its output even when redirected, and ignored `NO_COLOR`.**
  `Output::new` force-enabled colours on every run, which made the
  module's own environment rules dead code — so `dip status > report.txt`
  wrote ANSI escape codes into the file and `NO_COLOR=1` had no effect,
  despite being documented as honoured. Colours are now on only when
  stdout is a terminal; `NO_COLOR` turns them off, and
  `CLICOLOR_FORCE`/`FORCE_COLOR` keep them through a pipe
  (`dip status | less -R`). `--no-color` is unchanged.

- **The Engine API fast path could answer from the wrong daemon.**
  Socket discovery ignored `docker context`, so a machine with an active
  non-default context (OrbStack, Colima, a remote host) and a leftover
  `/var/run/docker.sock` from another engine would have `dip status`
  reporting one daemon's containers while every other command talked to
  another. Discovery now follows the CLI's own precedence — `DOCKER_HOST`
  → `DOCKER_CONTEXT` → `currentContext` in `~/.docker/config.json` →
  default — and an override it cannot use (`https://`, `ssh://`, a
  missing socket) falls back to the CLI instead of guessing a local
  socket. `tcp://` and `http://` endpoints are now spoken directly, so
  context-based and remote daemons get the fast path too.
- **Projects whose name needed normalizing showed as empty.** Compose
  labels containers with the project name lowercased *and* stripped of
  everything outside `[a-z0-9_-]`; the API fast path only lowercased it.
  A project in a directory like `My.App` or `client site` matched no
  label, and because the filter is applied server-side the daemon
  answered "no containers" rather than failing — so `dip status` printed
  an empty table instead of falling back. The normalization now matches
  compose's.
- Reaching an unreachable daemon no longer burns the full 2 s request
  budget before falling back: connecting has its own 300 ms limit.
- **`dip exec` and `dip shell` always exited 0.** The container command's
  exit code was read and then discarded, so `dip exec app phpunit`
  reported success however the tests went — and any CI step or `dip run`
  script built on it silently passed. Both now exit with the command's
  own code, as `dip db console` already did.
- **dip's own chatter went to stdout, landing in the middle of piped
  data.** `dip exec app pg_dump | gzip` got a `· Running '…'` line mixed
  into the dump, and `dip logs app > app.log` got a banner, separators,
  a service list and `· Ctrl+C to exit` above the log lines. Progress,
  confirmations, warnings and errors now go to stderr — where docker,
  compose and the existing spinner already put theirs — and stdout
  carries only the payload. Interactive output is unchanged; both
  streams still reach the terminal.

  What stays on stdout is what the command was asked to produce: every
  rendered report (`status`, `health`, `env`, `ls`, `db list`, `cert`,
  `sysinfo`, `doctor`, `validate`…), the `--format json` output, the
  path from `dip root`, the container's own output from `dip exec` and
  `dip logs`, and the public URL from `dip share` — which reads like a
  success notice but is the whole point of the command, so
  `dip share 8080 | grep -o 'https://[^ ]*'` keeps working. "Nothing
  found" answers (`dip status` with no containers, `dip ls` with no
  projects, `dip stats` with nothing running) stay on stdout too, so a
  redirect records them instead of leaving a table framed around a gap.
  A unit test pins which helper writes to which stream.
- **`dip shell`'s fallback to `sh` never ran.** The check tested for an
  `Err` that a passthrough stream never returns, so `dip shell` on an
  image without bash just failed. It now retries with `sh` on exit code
  126/127 — the shell reporting itself missing or not executable.

### Internal

- **Integration tests against a real daemon** (`scripts/integration-test.sh`,
  run by a new CI job on `ubuntu-latest`). Nineteen checks covering what
  unit tests structurally cannot see: whether a command runs once or twice,
  whether a TTY reaches the container, whether an exit code survives the
  trip, whether piped stdout stays free of dip's own output. Every serious
  bug fixed in this release was runtime-only and would have shipped
  otherwise. The suite was verified by reintroducing the double-execution
  bug and confirming it fails (`expected: 1, actual: 2`). It creates a
  throwaway alpine project and removes it, leaving nothing behind.

- Dependency refresh clearing `cargo audit`: `rust_decimal` 1.42.1 →
  1.43.0 drops its optional `rkyv` 0.7 (RUSTSEC-2026-0235 — an advisory
  against a crate that was in `Cargo.lock` but never actually compiled,
  since `Cargo.lock` records optional dependencies regardless of feature
  activation), and `chacha20` 0.10.1 → 0.10.2 leaves the yanked release.
  269 → 258 crate dependencies. Every direct dependency is now at its
  latest release; requirement floors were raised to match what resolves
  (`clap`/`clap_complete` 4.5 → 4.6, `rustls-pki-types` 1.9 → 1.15).
  `crypto-common` stays at 0.1.6, held back by the MSRV of `digest` 0.10
  under `sqlx`; no advisory affects it.
- A second refresh before tagging: `cargo update` lifts `rustls` 0.23.44 →
  0.23.46 (RUSTSEC-2026-0285, -0346, -0347, -0348, -0349), `rustls-webpki`
  0.103.15 → 0.103.16 (-0382), `webpki-roots` 1.0.9 → 1.0.11 (-0381),
  `ctutils` 0.4.2 → 0.4.8 (-0385) and `tokio` 1.53.1 → 1.54.1 (-0386,
  unsound `AsyncFd` constructors). The one advisory left, RUSTSEC-2026-0459
  against `libsqlite3-sys` 0.37, is the same lockfile artefact as `rkyv`
  above: `sqlx-sqlite` 0.9.0 pins it and nothing newer exists, while dip
  enables only `mysql` and `postgres`, so the crate is never compiled
  (`cargo tree -i libsqlite3-sys` is empty). `.cargo/audit.toml` ignores
  that one advisory with the reasoning inline, to be dropped when
  `sqlx-sqlite` moves to 0.39.
- `tokio`'s `io-std` feature added for the exec path's stdin pump. Binary
  size unchanged at 6.2 MB.
- `Output` grew a `report` helper for the one case the stdout/stderr split
  does not settle on its own: a line that *is* the command's answer but
  reads like commentary ("No containers found").

## 0.2.0 — 2026-08-03

Hardening release: no new surface beyond two commands, a lot of
stability, supply-chain and correctness work.

### Added

- `dip exec -w/--workdir <dir>` — run the command in a specific directory
  inside the container (maps to `docker compose exec -w`; the Apple
  Container shim translates it to `container exec --workdir`).
- `dip update-commands [template] [--dry-run]` — refresh scaffolded
  `.dip/commands/` scripts from the embedded templates: missing scripts
  are added, outdated ones updated (previous version saved under
  `.dip/commands.bak/`), custom scripts are never touched. The template
  is auto-detected and remembered in `.dip/.template` (also written by
  `dip init` from now on).
- CI: shellcheck over all template scripts, a lint against argv-joining
  `dip exec` wrappers, and a weekly `cargo audit` job.
- CHANGELOG (this file).

### Fixed

- **Template command wrappers broke shell metacharacters.** Scaffolded
  wrappers joined user arguments with `$*` into a single string that a
  container shell re-parsed — `drush php:eval "\Drupal::..."`,
  `psql -c "SELECT count(*)"` and anything with `()`, quotes or `;`
  failed. All wrappers now pass `"$@"` as separate argv. Existing
  projects can pick up the fix with `dip update-commands`.
- **`dip validate` / `dip explain` failed on any project using
  `depends_on`.** `docker compose config --format json` normalizes
  `depends_on` into a map with conditions; the config struct only
  accepted the list form. Both forms are accepted now, and the parse
  error message includes the underlying serde detail.
- **node-multi template generated an invalid `packageManager` field.**
  corepack rejects `pnpm@latest` (requires exact semver); the entrypoint
  now pins the activated pnpm version at workspace generation time.
- **Proxy: dead upstreams hung the browser.** A 5s connect timeout on
  both the pooled client and dedicated connections returns a clear 502
  instead of waiting out the ~75s OS TCP timeout. No response timeout on
  purpose — xdebug sessions legitimately hold requests for minutes.
- **Proxy: request bodies were fully buffered in RAM.** Bodies over
  16 MB (or chunked ones) now stream through a dedicated connection —
  a 300 MB upload peaks at ~7 MB of proxy RSS instead of ~300 MB.
- **Proxy: container start events were handled serially.** A burst of
  starting containers no longer waits behind each other's 400 ms
  networking-init pause; routes appear in parallel.
- **DNS: responses over 512 bytes were silently truncated.** Buffers
  raised to the EDNS0 4096 bytes — large upstream answers (DNSSEC,
  HTTPS/SVCB records) resolve reliably now.
- **DNS: forwarded queries accepted answers from any source.** The
  upstream socket is now `connect()`-ed and transaction IDs are
  verified, so off-path datagrams are dropped.

### Changed

- **Read-only `ps` queries go straight to the Docker Engine socket**
  instead of spawning the docker CLI: `dip status`/`health`/`stats` drop
  from ~105 ms to ~30 ms. Socket discovery: `DOCKER_HOST` →
  `/var/run/docker.sock` → OrbStack → Docker Desktop → colima; any
  failure falls back to the docker CLI silently.
- **Own minimal YAML parser** replaces the `noyalib` dependency for
  compose-file parsing. Covers the compose subset (block maps/sequences,
  scalars with core-schema inference, quotes, comments, multi-line flow
  collections, block scalars, anchors/aliases, `<<:` merge) and fails
  with a line-numbered error on unsupported constructs (tabs, tags,
  multi-document). Differential-tested against noyalib on every embedded
  template before the swap; fuzz-tested against garbage input.
- **Proxy is HTTP/1.1 only.** `h2` removed from ALPN and from the
  dependency tree — browsers fall back to HTTP/1.1, WebSocket upgrades
  still work, and the whole h2 stack leaves the binary. A local dev
  proxy gains nothing from HTTP/2.
- `tokio`/`hyper`/`hyper-util` `full` features narrowed to what is
  actually used; release profile now strips symbols and uses thin LTO.
  Binary size: 10.8 MB → 6.1 MB; dependency graph: 249 → 234 crates.
  `panic = "abort"` was deliberately **not** enabled: the proxy is a
  long-running daemon and tokio isolates panics per task only with
  unwinding (~1.3 MB cost — worth it).

### Internal

- Integration tests for the proxy pipeline on real sockets: routing,
  404s, pooled vs streaming body paths, fail-fast on dead upstreams.
- No-panic fuzz tests for the YAML parser and DNS packet parsers.
- Load-verified: 1000 TLS requests — RSS 6.3→7.1 MB, fd count stable;
  300 MB upload — 7.3 MB peak RSS; CLI startup ~4 ms.
