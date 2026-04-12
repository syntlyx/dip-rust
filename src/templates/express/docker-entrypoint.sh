#!/bin/sh
set -e

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
if [ ! -d "/app/node_modules" ] || [ -z "$(ls -A /app/node_modules)" ]; then
  echo "[entrypoint] Installing dependencies..."
  pnpm install
fi

exec "$@"
