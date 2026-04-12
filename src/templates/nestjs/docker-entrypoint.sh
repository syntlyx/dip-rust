#!/bin/sh
set -e

# Scaffold a fresh NestJS project if nothing is mounted yet
if [ ! -f "/app/package.json" ]; then
  echo "[entrypoint] No project found — scaffolding NestJS..."

  cd /tmp
  nest new project \
    --package-manager pnpm \
    --skip-git \
    --skip-install \
    --strict

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
