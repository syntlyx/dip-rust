#!/bin/sh
set -e

# Scaffold a fresh Django project if nothing is mounted yet.
# Scaffold into a temp dir first, then copy — avoids "file already exists"
# errors when /app has leftover files from a partial previous run.
if [ ! -f "/app/manage.py" ]; then
  echo "[entrypoint] No Django project found — scaffolding..."

  pip install django psycopg2-binary redis celery django-environ --quiet

  tmpdir=$(mktemp -d)
  django-admin startproject config "$tmpdir"
  cp -r "$tmpdir/." /app/
  rm -rf "$tmpdir"

  # Save requirements so subsequent starts skip reinstall
  cat >/app/requirements.txt <<'EOF'
django
psycopg2-binary
redis
celery
django-environ
EOF

  # Celery app module
  cat >/app/config/celery.py <<'EOF'
import os
from celery import Celery

os.environ.setdefault("DJANGO_SETTINGS_MODULE", "config.settings")

app = Celery("config")
app.config_from_object("django.conf:settings", namespace="CELERY")
app.autodiscover_tasks()
EOF

  # Wire celery app into the package so `-A config` finds it
  cat >/app/config/__init__.py <<'EOF'
from .celery import app as celery_app

__all__ = ("celery_app",)
EOF

  # Replace ALLOWED_HOSTS in Django settings
  SETTINGS_FILE="/app/config/settings.py"
  if ! grep -q "import os" "$SETTINGS_FILE"; then
    sed -i "s/from pathlib import Path/from pathlib import Path\\nimport os/" "$SETTINGS_FILE"
  fi
  if ! grep -q "os.getenv(\"DOMAIN\"" "$SETTINGS_FILE"; then
    sed -i "s/ALLOWED_HOSTS = .*/ALLOWED_HOSTS = (os.getenv(\"DOMAIN\", \"\").split(\",\") if os.getenv(\"DOMAIN\") else []) + ['127.0.0.1', 'localhost']/" "$SETTINGS_FILE"
  fi

  echo "[entrypoint] Scaffold done."
fi

# Install dependencies when requirements.txt exists but Django is not importable
if [ -f "/app/requirements.txt" ] && ! python -c "import django" 2>/dev/null; then
  echo "[entrypoint] Installing Python dependencies..."
  pip install -r /app/requirements.txt --quiet
fi

cd /app

# Run migrations only for the main app service (not celery worker)
if [ "$SKIP_MIGRATE" != "1" ]; then
  python manage.py migrate --run-syncdb
fi

exec "$@"
