#!/usr/bin/env sh
# Uninstall perfetto-mcp-rs and deregister it from Claude Code / Codex.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/uninstall.sh | sh
#
# Environment overrides:
#   INSTALL_DIR   Where the binary lives (default: $HOME/.local/bin)
#   SCOPE         Claude scope at install time: user|local|project (default:
#                 user). For local/project, run this script from the original
#                 project directory so the scoped entry is reachable.
#
# The binary's `uninstall` subcommand (v0.8+) owns deregistration + cache
# cleanup. This wrapper only handles:
#   (a) delegating to the binary when present and new enough,
#   (b) a best-effort manual-hint fallback for missing-binary / v0.7-binary
#       edge cases,
#   (c) removing the binary itself after a successful uninstall (a running
#       .exe can't delete itself on Windows).
#
# Preserves the binary on subcommand failure so the user can fix the issue
# (locked cache, wrong project dir, etc.) and re-run.

set -eu

INSTALL_DIR_OVERRIDDEN="${INSTALL_DIR:+yes}"
: "${INSTALL_DIR:=${HOME}/.local/bin}"
: "${SCOPE:=user}"

BIN_NAME="perfetto-mcp-rs"

info() { printf '==> %s\n' "$1"; }
warn() { printf 'warning: %s\n' "$1" >&2; }

detect_platform() {
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  case "$os" in
    linux)                printf 'linux' ;;
    darwin)               printf 'macos' ;;
    msys*|mingw*|cygwin*) printf 'windows' ;;
    *)                    printf 'linux' ;;  # best-effort
  esac
}

main() {
  platform="$(detect_platform)"
  bin_file="${BIN_NAME}"
  case "$platform" in
    windows) bin_file="${BIN_NAME}.exe" ;;
  esac

  info "Uninstalling ${BIN_NAME} from ${INSTALL_DIR} (scope=${SCOPE})"
  if [ "${INSTALL_DIR_OVERRIDDEN:-no}" = "yes" ]; then
    info "Using INSTALL_DIR=${INSTALL_DIR} (override). Must match the install-time value."
  fi

  removable=no

  if [ -e "${INSTALL_DIR}/${bin_file}" ]; then
    # Detect v0.8+ via top-level --help output: the binary prints its
    # subcommands there; v0.7 and older don't have `uninstall`. Do NOT use
    # `uninstall --help`: clap's --help is a short-circuit arg and would
    # exit 0 on v0.7 (treating "uninstall" as an unchecked positional).
    # grep -E needs POSIX [[:space:]] — `\s` isn't portable.
    help_status=0
    help_output="$("${INSTALL_DIR}/${bin_file}" --help 2>&1)" || help_status=$?

    if [ "$help_status" -eq 0 ]; then
      if printf '%s\n' "$help_output" | grep -qE '^[[:space:]]+uninstall([[:space:]]|$)'; then
        if "${INSTALL_DIR}/${bin_file}" uninstall --scope "$SCOPE"; then
          removable=yes
        else
          warn "binary uninstall failed under --scope ${SCOPE}; **keeping binary in place**"
          warn "for retry. Common causes: locked cache dir, or --scope local/project run"
          warn "from the wrong directory. Fix the issue and re-run uninstall.sh."
          # removable stays no
        fi
      else
        # v0.7 install.sh / install.ps1 only ever registered with --scope user
        # (no scope flag at all in those releases) — hardcode that here, even
        # if the current $SCOPE is local/project. Otherwise we'd hand the user
        # a command that looks right for v0.8 semantics but can't reach the
        # actual v0.7 user-scope entry.
        warn "old binary (v0.7) without 'uninstall' subcommand; manual cleanup needed:"
        warn "  claude mcp remove perfetto-mcp-rs --scope user"
        warn "  codex  mcp remove perfetto-mcp-rs"
        warn "  (cache: see CHANGELOG Migration section for 3-platform commands)"
        removable=yes
      fi
    else
      warn "installed binary exists but --help failed; **keeping it in place**"
      warn "for inspection/retry instead of misclassifying it as v0.7."
      printf '%s\n' "$help_output" | sed 's/^/    /' >&2
      # removable stays no
    fi
  else
    # Recovery: binary already removed/moved. Best-effort deregister via
    # the CLIs directly. Cache cleanup is NOT mirrored in shell — that's
    # Rust's single source of truth (`dirs::data_local_dir()`), and
    # repeating the 3-platform rules here is exactly what this refactor
    # is getting rid of. Print manual commands for the rare recovery case.
    warn "binary missing at ${INSTALL_DIR}/${bin_file}; running shell fallback for deregistration."
    # Hardcode --scope user here, regardless of the caller's $SCOPE. We
    # can't tell whether the missing binary was v0.7 (only registered
    # user-scope) or v0.8 (could be any scope); user-scope is the safe
    # common denominator. If the user actually had a local/project
    # registration, they need a `cd` + manual `claude mcp remove --scope X`
    # anyway because local/project entries are CWD-keyed.
    command -v claude >/dev/null 2>&1 \
      && claude mcp remove perfetto-mcp-rs --scope user 2>&1 | sed 's/^/    /'
    command -v codex >/dev/null 2>&1 \
      && codex mcp remove perfetto-mcp-rs 2>&1 | sed 's/^/    /'
    warn "If you also had a --scope local/project registration, from that project dir run:"
    warn "  claude mcp remove perfetto-mcp-rs --scope local    # or --scope project"
    warn "Cache not auto-cleaned (binary missing). Remove manually:"
    warn "  Linux:   rm -rf \"\${XDG_DATA_HOME:-\$HOME/.local/share}/perfetto-mcp-rs\""
    warn "  macOS:   rm -rf \"\$HOME/Library/Application Support/perfetto-mcp-rs\""
    warn "  Windows (Git Bash): rm -rf \"\$(cygpath -u \"\$LOCALAPPDATA\")/perfetto-mcp-rs\""
    warn "  Windows (PowerShell): Remove-Item -Recurse -Force \"\$env:LOCALAPPDATA\\perfetto-mcp-rs\""
    removable=yes  # nothing to remove
  fi

  if [ "$removable" = "yes" ]; then
    # Windows holds a DELETE lock on a running .exe; `rm -f` would fail.
    # `mv` is allowed on locked files, so rename aside first. The aside is
    # best-effort cleaned; if still locked, next install.sh will sweep it.
    #
    # Also sweep any pre-existing `.old-*` leftovers from earlier upgrades
    # (install.sh:128 does the same sweep on fresh installs; uninstall.ps1
    # does it too; this branch needs to match or upgraded-then-uninstalled
    # Git Bash users are left with hidden stale copies in INSTALL_DIR).
    case "$platform" in
      windows)
        if [ -e "${INSTALL_DIR}/${bin_file}" ]; then
          ts="$(date +%s)"
          mv "${INSTALL_DIR}/${bin_file}" "${INSTALL_DIR}/${bin_file}.old-${ts}" 2>/dev/null \
            || warn "could not rename ${bin_file} aside (still locked? close MCP clients and re-run)"
          rm -f "${INSTALL_DIR}/${bin_file}.old-${ts}" 2>/dev/null || true
        fi
        # Sweep leftover asides (best-effort; locked ones survive to next install sweep).
        for aside in "${INSTALL_DIR}/${bin_file}".old-*; do
          [ -e "$aside" ] || continue
          rm -f "$aside" 2>/dev/null || true
        done
        ;;
      *)
        rm -f "${INSTALL_DIR}/${bin_file}"
        ;;
    esac
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
