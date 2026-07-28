#!/bin/sh
set -e

# Scaffold a minimal Go project if nothing is mounted yet
if [ ! -f "/app/go.mod" ]; then
  echo "[entrypoint] No Go project found — scaffolding..."

  cd /app
  go mod init "${PROJECT_NAME}"

  mkdir -p cmd/api
  cat >cmd/api/main.go <<'GOEOF'
package main

import (
	"fmt"
	"log"
	"net/http"
)

func main() {
	http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		fmt.Fprintln(w, "Hello from Go!")
	})
	http.HandleFunc("/health", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		fmt.Fprintln(w, `{"status":"ok"}`)
	})

	log.Println("Starting server on :8080")
	log.Fatal(http.ListenAndServe(":8080", nil))
}
GOEOF

  # Air config for hot-reload
  cat >.air.toml <<'AIREOF'
root = "."
tmp_dir = "tmp"

[build]
cmd = "go build -o ./tmp/main ./cmd/api"
bin = "tmp/main"
include_ext = ["go"]
exclude_dir = ["tmp", "vendor"]
AIREOF

  echo "[entrypoint] Scaffold done."
fi

exec "$@"
