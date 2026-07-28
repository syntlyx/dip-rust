# Changelog

## 0.2.0 — unreleased

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
