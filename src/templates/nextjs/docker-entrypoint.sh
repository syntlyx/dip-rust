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
  "@swc/core": true
  "@tailwindcss/oxide": true
  esbuild: true
  sharp: true
  unrs-resolver: true
YAML
  fi
}

# Scaffold a fresh Next.js project if nothing is mounted yet
if [ ! -f "/app/package.json" ]; then
  echo "[entrypoint] No project found — scaffolding Next.js..."

  cd /tmp
  pnpm dlx \
    --allow-build=sharp \
    --allow-build=unrs-resolver \
    --allow-build=@swc/core \
    --allow-build=@tailwindcss/oxide \
    create-next-app@latest project \
    --ts \
    --tailwind \
    --eslint \
    --app \
    --src-dir \
    --import-alias "@/*" \
    --skip-install \
    --no-git

  sed -i \
    "s|/\* config options here \*/|/* config options here */\n  allowedDevOrigins: [\"${DOMAIN}\"],|" \
    /tmp/project/next.config.ts

  cp -r /tmp/project/. /app/
  echo "[entrypoint] Scaffold done."
fi

cd /app

# Install deps if node_modules is missing (first clone / fresh volume)
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
