#!/bin/sh
set -e

# Scaffold a fresh Angular project if nothing is mounted yet
if [ ! -f "/app/package.json" ]; then
  echo "[entrypoint] No Angular project found — scaffolding..."

  cd /tmp
  pnpm dlx @angular/cli@latest new project \
    --directory project \
    --skip-git \
    --skip-install \
    --style=css \
    --routing=true \
    --package-manager=pnpm

  # Patch angular.json to allow the dip domain (idempotent)
  if [ -n "$DOMAIN" ] && [ -f "/tmp/project/angular.json" ]; then
    node -e "
    const fs = require('fs');
    const domain = process.env.DOMAIN;
    const cfg = JSON.parse(fs.readFileSync('/tmp/project/angular.json', 'utf8'));
    const appName = Object.keys(cfg.projects)[0];
    const serve = cfg.projects[appName].architect.serve;
    if (!serve.options) serve.options = {};
    const hosts = serve.options.allowedHosts || [];
    if (!hosts.includes(domain)) {
      serve.options.allowedHosts = [...hosts, domain];
      fs.writeFileSync('/tmp/project/angular.json', JSON.stringify(cfg, null, 2));
      console.log('[entrypoint] Added ' + domain + ' to allowedHosts');
    }
  "
  fi

  cp -r /tmp/project/. /app/
  echo "[entrypoint] Scaffold done."
fi

cd /app

# Install deps if node_modules is missing
if [ ! -d "/app/node_modules" ] || [ -z "$(ls -A /app/node_modules)" ]; then
  echo "[entrypoint] Installing dependencies..."
  pnpm install
fi

exec "$@"
