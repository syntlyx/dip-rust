#!/bin/sh
set -e

# Download WordPress core if not present
if [ ! -f "/var/www/html/wp-login.php" ]; then
  echo "[entrypoint] No WordPress found — downloading latest core..."

  wp core download \
    --allow-root \
    --path=/var/www/html

  echo "[entrypoint] WordPress core downloaded."
fi

# Generate wp-config.php if not present
if [ ! -f "/var/www/html/wp-config.php" ]; then
  echo "[entrypoint] Generating wp-config.php..."

  wp config create \
    --allow-root \
    --path=/var/www/html \
    --dbname="${MYSQL_DATABASE}" \
    --dbuser="${MYSQL_USER}" \
    --dbpass="${MYSQL_PASSWORD}" \
    --dbhost=db \
    --skip-check
fi

# Fix permissions
chown -R www-data:www-data /var/www/html/wp-content 2>/dev/null || true

exec "$@"
