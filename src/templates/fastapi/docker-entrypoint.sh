#!/bin/sh
set -e

# Scaffold a minimal FastAPI project if nothing is mounted yet
if [ ! -f "/app/app/main.py" ]; then
  echo "[entrypoint] No FastAPI project found — scaffolding..."

  mkdir -p /app/app
  cat >/app/app/__init__.py <<'EOF'
EOF
  cat >/app/app/main.py <<'EOF'
from fastapi import FastAPI

app = FastAPI()

@app.get("/")
async def root():
    return {"message": "Hello from FastAPI!"}

@app.get("/health")
async def health():
    return {"status": "ok"}
EOF
  cat >/app/requirements.txt <<'EOF'
fastapi[standard]
uvicorn[standard]
sqlalchemy
asyncpg
alembic
redis
python-dotenv
EOF

  echo "[entrypoint] Scaffold done."
fi

# Install deps if not installed
if [ ! -d "/app/.venv" ]; then
  echo "[entrypoint] Installing Python dependencies..."
  uv venv /app/.venv
  uv pip install -r /app/requirements.txt --python /app/.venv/bin/python --quiet
fi

export PATH="/app/.venv/bin:$PATH"

exec "$@"
