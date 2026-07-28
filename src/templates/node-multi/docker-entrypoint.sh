#!/bin/sh
set -e

configure_pnpm_install() {
  if ! grep -q '^confirmModulesPurge:' pnpm-workspace.yaml; then
    cat >>pnpm-workspace.yaml <<'YAML'

confirmModulesPurge: false
YAML
  fi

  if ! grep -q '^allowBuilds:' pnpm-workspace.yaml; then
    cat >>pnpm-workspace.yaml <<'YAML'

allowBuilds:
  esbuild: true
  sharp: true
  unrs-resolver: true
YAML
  fi
}

cd /app

if [ ! -f "/app/package.json" ]; then
  echo "[entrypoint] No package.json found; creating a minimal pnpm workspace..."

  # corepack rejects dist-tags in packageManager; pin the activated version
  PNPM_VERSION="$(pnpm --version)"

  cat > package.json <<JSON
{
  "name": "workspace",
  "private": true,
  "packageManager": "pnpm@${PNPM_VERSION}",
  "scripts": {
    "dev:web": "pnpm --filter web dev",
    "dev:api": "pnpm --filter api dev",
    "dev:worker": "pnpm --filter worker dev"
  }
}
JSON

  cat > pnpm-workspace.yaml <<'YAML'
packages:
  - "apps/*"
YAML

  mkdir -p apps/web apps/api apps/worker

  cat > apps/web/package.json <<'JSON'
{
  "name": "web",
  "private": true,
  "scripts": {
    "dev": "node server.js"
  }
}
JSON

  cat > apps/web/server.js <<'JS'
const http = require("node:http");
const port = Number(process.env.APP_PORT || 3000);
const appId = process.env.APP_ID || "web";

http
  .createServer((req, res) => {
    res.setHeader("content-type", "application/json");
    res.end(JSON.stringify({ app: appId, path: req.url }));
  })
  .listen(port, "0.0.0.0", () => {
    console.log(`[${appId}] listening on ${port}`);
  });
JS

  cp apps/web/package.json apps/api/package.json
  sed -i 's/"name": "web"/"name": "api"/' apps/api/package.json
  cp apps/web/server.js apps/api/server.js

  cat > apps/worker/package.json <<'JSON'
{
  "name": "worker",
  "private": true,
  "scripts": {
    "dev": "node worker.js"
  }
}
JSON

  cat > apps/worker/worker.js <<'JS'
const appId = process.env.APP_ID || "worker";
console.log(`[${appId}] started`);
setInterval(() => {
  console.log(`[${appId}] heartbeat ${new Date().toISOString()}`);
}, 10000);
JS
fi

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
