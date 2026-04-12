#!/bin/sh
set -e

# Scaffold a fresh Nuxt project if nothing is mounted yet
if [ ! -f "/app/package.json" ]; then
  echo "[entrypoint] No Nuxt project found — scaffolding..."

  cd /tmp
  pnpm dlx nuxi@latest init project \
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
if [ ! -d "/app/node_modules" ] || [ -z "$(ls -A /app/node_modules)" ]; then
  echo "[entrypoint] Installing dependencies..."
  pnpm install
fi

exec "$@"
