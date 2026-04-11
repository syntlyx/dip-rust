#!/usr/bin/env bash
# dip installer
# Usage: curl -fsSL https://raw.githubusercontent.com/syntlyx/dip-rust/main/install.sh | bash
#        curl -fsSL ... | bash -s -- --version 1.2.3
#        curl -fsSL ... | bash -s -- --uninstall

set -euo pipefail

# ─── config ───────────────────────────────────────────────────────────────────

REPO_OWNER="syntlyx"
REPO_NAME="dip-rust"
BIN_NAME="dip"
BIN_DIR="${HOME}/.local/bin"
VERSION="${DIP_VERSION:-latest}"
UNINSTALL=false

# ─── colors ───────────────────────────────────────────────────────────────────

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

info()    { echo -e "${GREEN}[INFO]${NC} $*"; }
warn()    { echo -e "${YELLOW}[WARN]${NC} $*"; }
error()   { echo -e "${RED}[ERROR]${NC} $*" >&2; }
step()    { echo -e "\n${CYAN}==>${NC} $*"; }

# ─── args ─────────────────────────────────────────────────────────────────────

while [[ $# -gt 0 ]]; do
  case $1 in
    -v|--version)  VERSION="$2"; shift 2 ;;
    --uninstall)   UNINSTALL=true; shift ;;
    -h|--help)
      echo "Usage: install.sh [-v VERSION] [--uninstall]"
      exit 0 ;;
    *) error "Unknown option: $1"; exit 1 ;;
  esac
done

# ─── uninstall ────────────────────────────────────────────────────────────────

uninstall() {
  step "Uninstalling ${BIN_NAME}"
  rm -f "${BIN_DIR}/${BIN_NAME}"
  info "Removed ${BIN_DIR}/${BIN_NAME}"
  info "Done. Shell rc file was not touched — remove PATH entry manually if needed."
}

# ─── platform detection ───────────────────────────────────────────────────────

detect_target() {
  local os arch
  os=$(uname -s)
  arch=$(uname -m)

  case "${os}-${arch}" in
    Darwin-arm64)   echo "aarch64-apple-darwin" ;;
    Darwin-x86_64)  echo "x86_64-apple-darwin" ;;
    Linux-aarch64)  echo "aarch64-unknown-linux-musl" ;;
    Linux-x86_64)   echo "x86_64-unknown-linux-musl" ;;
    *)
      error "Unsupported platform: ${os} ${arch}"
      exit 1 ;;
  esac
}

# ─── download ─────────────────────────────────────────────────────────────────

download() {
  local url="$1" dest="$2"
  if command -v curl &>/dev/null; then
    curl -fSL --progress-bar "$url" -o "$dest"
  elif command -v wget &>/dev/null; then
    wget -q --show-progress "$url" -O "$dest"
  else
    error "curl or wget is required"
    exit 1
  fi
}

resolve_url() {
  local target="$1"
  local asset="${BIN_NAME}-${target}"

  if [[ "$VERSION" == "latest" ]]; then
    echo "https://github.com/${REPO_OWNER}/${REPO_NAME}/releases/latest/download/${asset}"
  else
    local ver="${VERSION#v}"  # strip leading v if present
    echo "https://github.com/${REPO_OWNER}/${REPO_NAME}/releases/download/v${ver}/${asset}"
  fi
}

# ─── PATH setup ───────────────────────────────────────────────────────────────

ensure_path() {
  if [[ ":${PATH}:" == *":${BIN_DIR}:"* ]]; then
    return
  fi

  local rc
  case "$(basename "${SHELL:-bash}")" in
    zsh)  rc="${HOME}/.zshrc" ;;
    fish) rc="${HOME}/.config/fish/config.fish" ;;
    *)    rc="${HOME}/.bashrc" ;;
  esac

  {
    echo ""
    echo "# Added by dip installer"
    echo "export PATH=\"${BIN_DIR}:\$PATH\""
  } >> "$rc"

  warn "Added ${BIN_DIR} to PATH in ${rc}"
  warn "Restart your shell or run: source ${rc}"
}

# ─── main ─────────────────────────────────────────────────────────────────────

main() {
  if [[ "$UNINSTALL" == true ]]; then
    uninstall
    exit 0
  fi

  local target
  target=$(detect_target)

  step "Installing ${CYAN}${BIN_NAME}${NC} (${target})"

  mkdir -p "$BIN_DIR"

  local url tmp
  url=$(resolve_url "$target")
  tmp=$(mktemp)

  info "Downloading: ${url}"
  download "$url" "$tmp"

  chmod +x "$tmp"
  mv "$tmp" "${BIN_DIR}/${BIN_NAME}"

  ensure_path

  echo ""
  info "Installed: ${BIN_DIR}/${BIN_NAME}"
  info "Version:   $("${BIN_DIR}/${BIN_NAME}" --version 2>/dev/null || echo "unknown")"
  echo ""
  echo "  Get started:"
  echo "    dip init      — scaffold a new project"
  echo "    dip --help    — all commands"
  echo ""
}

main "$@"
