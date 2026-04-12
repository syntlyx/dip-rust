#!/bin/sh
set -e

# Scaffold a fresh Next.js project if nothing is mounted yet
if [ ! -f "/app/package.json" ]; then
  echo "[entrypoint] No project found — scaffolding Next.js..."

  cd /tmp
  pnpm dlx create-next-app@latest project \
    --ts \
    --tailwind \
    --eslint \
    --app \
    --src-dir \
    --import-alias "@/*" \
    --no-git

  sed -i \
    "s|/\* config options here \*/|/* config options here */\n  allowedDevOrigins: [\"${DOMAIN}\"],|" \
    /tmp/project/next.config.ts

  cp -r /tmp/project/. /app/
  echo "[entrypoint] Scaffold done."
fi

cd /app

# Install deps if node_modules is missing (first clone / fresh volume)
if [ ! -d "/app/node_modules" ] || [ -z "$(ls -A /app/node_modules)" ]; then
  echo "[entrypoint] Installing dependencies..."
  pnpm install
fi

exec "$@"
