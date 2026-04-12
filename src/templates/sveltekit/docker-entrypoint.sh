#!/bin/sh
set -e

# Scaffold a fresh SvelteKit project if nothing is mounted yet
if [ ! -f "/app/package.json" ]; then
  echo "[entrypoint] No SvelteKit project found — scaffolding..."

  cd /tmp
  pnpm dlx sv@latest create project \
    --template minimal \
    --types ts \
    --no-add-ons \
    --no-install

  sed -i \
    -e 's/plugins: \[sveltekit()\]/plugins: [sveltekit()],/' \
    -e '/plugins: \[sveltekit()\],/a\  server: {\n    allowedHosts: true,\n  },' \
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
