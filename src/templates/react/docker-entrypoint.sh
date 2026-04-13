#!/bin/sh
set -e

# Scaffold a fresh React + Vite project if nothing is mounted yet
if [ ! -f "/app/package.json" ]; then
  echo "[entrypoint] No React project found — scaffolding..."

  cd /tmp
  pnpm create vite@latest project \
    --template react-ts

  sed -i \
    -e '/plugins: \[react()\],/a\  server: {\n    allowedHosts: true,\n  },' \
    /tmp/project/vite.config.ts

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
