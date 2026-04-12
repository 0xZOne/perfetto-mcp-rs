#!/usr/bin/env sh
# Install perfetto-mcp-rs and register it with Claude Code.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/install.sh | sh
#
# Environment overrides:
#   INSTALL_DIR   Where to place the binary (default: $HOME/.local/bin)
#   REPO          GitHub slug to download from (default: 0xZOne/perfetto-mcp-rs)
#   VERSION       Release tag to install (default: latest)

set -eu

: "${REPO:=0xZOne/perfetto-mcp-rs}"
: "${INSTALL_DIR:=${HOME}/.local/bin}"
: "${VERSION:=latest}"

BIN_NAME="perfetto-mcp-rs"

err() { printf 'error: %s\n' "$1" >&2; exit 1; }
info() { printf '==> %s\n' "$1"; }
warn() { printf 'warning: %s\n' "$1" >&2; }

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || err "$1 is required but not installed"
}

detect_platform() {
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  arch="$(uname -m)"
  case "$os" in
    linux)  os_tag="linux" ;;
    darwin) os_tag="mac" ;;
    msys*|mingw*|cygwin*)
      err "Windows not supported by this script. Download the .exe from https://github.com/${REPO}/releases" ;;
    *) err "unsupported OS: $os" ;;
  esac
  case "$arch" in
    x86_64|amd64)  arch_tag="amd64" ;;
    aarch64|arm64) arch_tag="arm64" ;;
    *) err "unsupported architecture: $arch" ;;
  esac
  printf '%s-%s' "$os_tag" "$arch_tag"
}

resolve_version() {
  if [ "$VERSION" != "latest" ]; then
    printf '%s' "$VERSION"
    return
  fi
  curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep -o '"tag_name":[[:space:]]*"[^"]*"' \
    | head -1 \
    | sed 's/.*"\([^"]*\)"$/\1/'
}

register_with_claude() {
  bin_path="$1"
  if ! command -v claude >/dev/null 2>&1; then
    cat <<EOF

NOTE: 'claude' CLI not found. To use this server with Claude Code, install
Claude Code first, then run:

    claude mcp add perfetto-mcp-rs --scope user ${bin_path}

EOF
    return
  fi
  # Idempotent: remove stale entry first, then add. Remove may fail harmlessly.
  claude mcp remove perfetto-mcp-rs --scope user >/dev/null 2>&1 || true
  if claude mcp add perfetto-mcp-rs --scope user "$bin_path" >/dev/null 2>&1; then
    info "Registered with Claude Code (user scope). Restart Claude Code to pick it up."
  else
    warn "Failed to register with Claude Code. Run manually:"
    printf '    claude mcp add perfetto-mcp-rs --scope user %s\n' "$bin_path"
  fi
}

main() {
  need_cmd curl
  need_cmd uname
  need_cmd install

  platform="$(detect_platform)"
  asset="${BIN_NAME}-${platform}"

  info "Detecting latest release from github.com/${REPO}"
  tag="$(resolve_version)"
  [ -n "$tag" ] || err "could not resolve release tag"
  info "Installing ${BIN_NAME} ${tag} (${platform})"

  url="https://github.com/${REPO}/releases/download/${tag}/${asset}"
  tmp="$(mktemp)"
  trap 'rm -f "$tmp"' EXIT INT TERM
  curl -fsSL --retry 3 -o "$tmp" "$url" \
    || err "download failed: ${url}"

  mkdir -p "$INSTALL_DIR"
  install -m 0755 "$tmp" "${INSTALL_DIR}/${BIN_NAME}"
  info "Installed to ${INSTALL_DIR}/${BIN_NAME}"

  case ":$PATH:" in
    *":${INSTALL_DIR}:"*) ;;
    *)
      cat <<EOF

NOTE: ${INSTALL_DIR} is not on your PATH. Add this to your shell rc:

    export PATH="${INSTALL_DIR}:\$PATH"

EOF
      ;;
  esac

  register_with_claude "${INSTALL_DIR}/${BIN_NAME}"
}

main "$@"
