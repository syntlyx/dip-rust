#!/bin/sh
set -e

# Scaffold a fresh Laravel project if nothing is mounted yet
if [ ! -f "/var/www/html/artisan" ]; then
  echo "[entrypoint] No Laravel project found — scaffolding..."

  composer create-project laravel/laravel /tmp/laravel \
    --prefer-dist \
    --no-interaction

  cp -r /tmp/laravel/. /var/www/html/
  echo "[entrypoint] Scaffold done."
fi

# Install PHP deps if vendor is missing (first clone / fresh volume)
if [ ! -d "/var/www/html/vendor" ]; then
  echo "[entrypoint] Installing PHP dependencies..."
  composer install --working-dir=/var/www/html
fi

# Install Node deps if node_modules is missing
if [ ! -d "/var/www/html/node_modules" ]; then
  echo "[entrypoint] Installing Node dependencies..."
  cd /var/www/html && pnpm install
fi

cd /var/www/html

# Generate app key if not set
if [ -z "${APP_KEY}" ]; then
  echo "[entrypoint] Generating APP_KEY..."
  php artisan key:generate --force
fi

# Fix storage permissions
chmod -R 775 storage bootstrap/cache
chown -R www-data:www-data storage bootstrap/cache

exec "$@"
