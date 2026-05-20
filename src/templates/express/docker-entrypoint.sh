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
}

# Scaffold a minimal Express project if nothing is mounted yet
if [ ! -f "/app/package.json" ]; then
  echo "[entrypoint] No project found — scaffolding Express..."

  mkdir -p /app/src
  cat >/app/package.json <<EOF
{
  "name": "${PROJECT_NAME}",
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "node --watch src/index.js",
    "start": "node src/index.js"
  },
  "dependencies": {
    "express": "^5.0.0"
  }
}
EOF

  cat >/app/src/index.js <<'JSEOF'
import express from 'express'

const app = express()
const port = process.env.PORT || 3000

app.use(express.json())

app.get('/', (req, res) => {
  res.json({ message: 'Hello from Express!' })
})

app.get('/health', (req, res) => {
  res.json({ status: 'ok' })
})

app.listen(port, '0.0.0.0', () => {
  console.log(`Server running on port ${port}`)
})
JSEOF

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
