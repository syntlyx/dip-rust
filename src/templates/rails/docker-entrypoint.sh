#!/bin/sh
set -e

if [ ! -f "/app/Gemfile" ]; then
  echo "[entrypoint] No Rails project found — scaffolding..."

  gem install rails --quiet --no-document
  cd /tmp
  rails new app \
    --database=postgresql \
    --skip-git \
    --asset-pipeline=propshaft \
    --javascript=importmap

  # Ensure tzinfo-data is available unconditionally on Linux
  sed -i '/platforms :mingw.*tzinfo-data/d' /tmp/app/Gemfile
  echo 'gem "tzinfo-data"' >>/tmp/app/Gemfile

  bundle install --gemfile=/tmp/app/Gemfile --quiet

  cp -r /tmp/app/. /app/
  echo "[entrypoint] Scaffold done."
fi

cd /app

if [ ! -d "/app/vendor/bundle" ]; then
  echo "[entrypoint] Installing gems..."
  bundle install
fi

rm -f tmp/pids/server.pid

exec "$@"
