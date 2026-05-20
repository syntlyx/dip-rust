#!/bin/sh
set -e

configure_pnpm_install() {
  if [ ! -f "pnpm-workspace.yaml" ]; then
    cat >pnpm-workspace.yaml <<'YAML'
packages:
  - "."
YAML
  fi

  if ! grep -q '^confirmModulesPurge:' pnpm-workspace.yaml; then
    cat >>pnpm-workspace.yaml <<'YAML'

confirmModulesPurge: false
YAML
  fi

  if ! grep -q '^allowBuilds:' pnpm-workspace.yaml; then
    cat >>pnpm-workspace.yaml <<'YAML'

allowBuilds:
  "@angular/build": true
  "@parcel/watcher": true
  esbuild: true
YAML
  fi
}

# Scaffold a fresh Angular project if nothing is mounted yet
if [ ! -f "/app/package.json" ]; then
  echo "[entrypoint] No Angular project found — scaffolding..."

  cd /tmp
  pnpm dlx \
    --allow-build=@angular/build \
    --allow-build=@parcel/watcher \
    --allow-build=esbuild \
    @angular/cli@latest new project \
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
if [ -d "/app/node_modules" ] && [ ! -f "/app/node_modules/.modules.yaml" ]; then
  echo "[entrypoint] Incomplete node_modules found; reinstalling dependencies..."
  find /app/node_modules -mindepth 1 -maxdepth 1 -exec rm -rf {} +
fi
if [ ! -d "/app/node_modules" ] || [ -z "$(ls -A /app/node_modules 2>/dev/null)" ]; then
  echo "[entrypoint] Installing dependencies..."
  configure_pnpm_install
  CI=true pnpm install --store-dir "${PNPM_STORE_DIR:-/pnpm/store}"
fi

exec "$@"
