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
  "@parcel/watcher": true
  esbuild: true
YAML
  fi
}

# Scaffold a fresh Nuxt project if nothing is mounted yet
if [ ! -f "/app/package.json" ]; then
  echo "[entrypoint] No Nuxt project found — scaffolding..."

  cd /tmp
  pnpm dlx \
    --allow-build=esbuild \
    --allow-build=@parcel/watcher \
    nuxi@latest init project \
    --template minimal \
    --packageManager pnpm \
    --no-install \
    --gitInit=false \
    --modules="" \
    --force

  sed -i \
    -e '/^})/i\  vite: {' \
    -e '/^})/i\    server: {' \
    -e '/^})/i\      allowedHosts: true,' \
    -e '/^})/i\    },' \
    -e '/^})/i\  },' \
    /tmp/project/nuxt.config.ts

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
