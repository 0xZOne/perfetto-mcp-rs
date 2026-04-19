#!/usr/bin/env sh
# Uninstall perfetto-mcp-rs and deregister it from Claude Code / Codex.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/uninstall.sh | sh
#
# Environment overrides:
#   INSTALL_DIR   Where the binary lives (default: $HOME/.local/bin)
#
# Idempotent: missing CLI / missing binary / empty cache dir are silent
# no-ops. claude/codex `mcp remove` failures (including "never registered")
# are surfaced with their raw output so users can distinguish benign cases
# from real config errors. PATH is intentionally not modified — see README.

set -eu

# Capture whether the user explicitly set INSTALL_DIR before we coalesce the
# default — used below to suppress the "match install-time INSTALL_DIR" hint
# for the common case where neither install nor uninstall touched it.
INSTALL_DIR_OVERRIDDEN="${INSTALL_DIR:+yes}"
: "${INSTALL_DIR:=${HOME}/.local/bin}"

BIN_NAME="perfetto-mcp-rs"

info() { printf '==> %s\n' "$1"; }
warn() { printf 'warning: %s\n' "$1" >&2; }

detect_platform() {
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  case "$os" in
    linux)                printf 'linux' ;;
    darwin)               printf 'macos' ;;
    msys*|mingw*|cygwin*) printf 'windows' ;;
    *)                    printf 'linux' ;;  # best-effort fallback
  esac
}

# Cache directory used by src/download.rs (`dirs::data_local_dir()` + "perfetto-mcp-rs").
# Empty output means "skip cache cleanup" — the caller treats that as a no-op.
# Trailing `return 0` is load-bearing: under `set -e` (POSIX sh / dash), a
# failing `[ -n "$x" ] && printf ...` chain at the function tail would make
# the function exit non-zero, which trips errexit on `cache="$(cache_dir ...)"`
# and aborts the entire uninstall before "Done." prints.
cache_dir() {
  case "$1" in
    linux)
      base="${XDG_DATA_HOME:-}"
      [ -n "$base" ] || { [ -n "${HOME:-}" ] && base="${HOME}/.local/share"; }
      [ -n "$base" ] && printf '%s/perfetto-mcp-rs' "$base"
      ;;
    macos)
      [ -n "${HOME:-}" ] && printf '%s/Library/Application Support/perfetto-mcp-rs' "$HOME"
      ;;
    windows)
      base="${LOCALAPPDATA:-}"
      # Git Bash / MSYS2 / Cygwin expose LOCALAPPDATA as a native Windows path
      # (e.g. C:\Users\Foo\AppData\Local). POSIX `[ -d ]` and `rm -rf` won't
      # walk that — convert to a unix-form path (/c/Users/Foo/...) so the
      # caller's directory test and removal actually hit the right tree.
      if [ -n "$base" ] && command -v cygpath >/dev/null 2>&1; then
        base="$(cygpath -u "$base" 2>/dev/null || printf '%s' "$base")"
      fi
      [ -n "$base" ] && printf '%s/perfetto-mcp-rs' "$base"
      ;;
  esac
  return 0
}

deregister_from_claude() {
  if ! command -v claude >/dev/null 2>&1; then
    cat <<EOF

NOTE: 'claude' CLI not found. If you registered the server with Claude Code
on another machine or before installing the CLI, run:

    claude mcp remove perfetto-mcp-rs --scope user

EOF
    return
  fi
  # We can't tell "server never registered" (benign) apart from "config broken"
  # (real failure) just from the exit code, so capture and surface the output
  # on non-zero. Letting the user judge beats silently lying about success.
  output="$(claude mcp remove perfetto-mcp-rs --scope user 2>&1)" && claude_exit=0 || claude_exit=$?
  if [ "$claude_exit" -eq 0 ]; then
    info "Deregistered from Claude Code (user scope). Restart Claude Code to drop it."
  else
    warn "claude mcp remove exited ${claude_exit}. Output:"
    printf '%s\n' "$output" | sed 's/^/    /'
    warn "Safe to ignore if perfetto-mcp-rs was never registered. Otherwise verify with: claude mcp list"
  fi
}

deregister_from_codex() {
  if ! command -v codex >/dev/null 2>&1; then
    cat <<EOF

NOTE: 'codex' CLI not found. If you registered the server with Codex on
another machine or before installing the CLI, run:

    codex mcp remove perfetto-mcp-rs

EOF
    return
  fi
  output="$(codex mcp remove perfetto-mcp-rs 2>&1)" && codex_exit=0 || codex_exit=$?
  if [ "$codex_exit" -eq 0 ]; then
    info "Deregistered from Codex. New Codex sessions will drop it."
  else
    warn "codex mcp remove exited ${codex_exit}. Output:"
    printf '%s\n' "$output" | sed 's/^/    /'
    warn "Safe to ignore if perfetto-mcp-rs was never registered. Otherwise verify with: codex mcp list"
  fi
}

remove_binary() {
  platform="$1"
  bin_file="${BIN_NAME}"
  case "$platform" in
    windows) bin_file="${BIN_NAME}.exe" ;;
  esac

  target="${INSTALL_DIR}/${bin_file}"

  if [ -e "$target" ]; then
    if rm -f "$target" 2>/dev/null; then
      info "Removed ${target}"
    else
      warn "Could not remove ${target} (is an MCP client still running it? close Claude Code / Codex and retry)"
    fi
  else
    info "No binary at ${target} (already removed or never installed)."
  fi

  # Sweep aside files left by prior installs on Windows.
  case "$platform" in
    windows)
      # Loop with for-glob; if no match, the literal pattern stays and we skip it.
      for aside in "${INSTALL_DIR}/${bin_file}".old-*; do
        [ -e "$aside" ] || continue
        if rm -f "$aside" 2>/dev/null; then
          info "Removed aside ${aside}"
        else
          warn "Could not remove ${aside} (still locked?)"
        fi
      done
      ;;
  esac
}

main() {
  platform="$(detect_platform)"

  info "Uninstalling ${BIN_NAME} from ${INSTALL_DIR}"
  if [ "${INSTALL_DIR_OVERRIDDEN:-no}" = "yes" ]; then
    info "Using INSTALL_DIR=${INSTALL_DIR} (override). If you customized INSTALL_DIR at install time, make sure this matches."
  fi

  deregister_from_claude
  deregister_from_codex
  remove_binary "$platform"

  cache="$(cache_dir "$platform")"
  if [ -z "$cache" ]; then
    info "Skipping cache cleanup (could not determine cache directory; HOME or LOCALAPPDATA unset?)."
  elif [ -d "$cache" ]; then
    if rm -rf "$cache" 2>/dev/null; then
      info "Removed cache ${cache}"
    else
      warn "Could not remove cache ${cache}"
    fi
  else
    info "No cache at ${cache} (already removed or never downloaded)."
  fi

  case ":$PATH:" in
    *":${INSTALL_DIR}:"*)
      cat <<EOF

NOTE: ${INSTALL_DIR} is still on your PATH. If you added it to your shell rc
when installing, remove that line manually (other tools may still depend on
this directory).

EOF
      ;;
  esac

  info "Done."
}

main "$@"
