#!/bin/sh
set -e

# Scaffold a fresh Drupal project if nothing is mounted yet
if [ ! -f "/var/www/html/composer.json" ]; then
  echo "[entrypoint] No Drupal project found — scaffolding Drupal 11..."

  composer create-project drupal/recommended-project /tmp/drupal \
    --no-interaction \
    --prefer-dist

  # Add drush as a project dependency
  composer --working-dir=/tmp/drupal require drush/drush --no-interaction

  cp -r /tmp/drupal/. /var/www/html/
  echo "[entrypoint] Scaffold done."
fi

# Install deps if vendor is missing (first clone / fresh volume)
if [ ! -d "/var/www/html/vendor" ]; then
  echo "[entrypoint] Installing dependencies..."
  composer install --working-dir=/var/www/html
fi

# Fix settings.php if not present
if [ ! -f "/var/www/html/web/sites/default/settings.php" ]; then
  echo "[entrypoint] Copying default settings.php..."
  cp /var/www/html/web/sites/default/default.settings.php \
    /var/www/html/web/sites/default/settings.php
  chmod 666 /var/www/html/web/sites/default/settings.php
fi

# Create files directory
mkdir -p /var/www/html/web/sites/default/files
chmod -R 777 /var/www/html/web/sites/default/files

exec "$@"
