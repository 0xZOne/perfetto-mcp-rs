# Uninstall perfetto-mcp-rs and deregister it from Claude Code / Codex.
#
# Usage:
#   irm https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/uninstall.ps1 | iex
#
# Environment overrides:
#   $env:INSTALL_DIR   Where the binary lives (default: $HOME\.local\bin)
#   $env:SCOPE         Claude scope at install time: user|local|project
#                      (default: user). For local/project, run from the
#                      original project directory.
#
# The binary's `uninstall` subcommand (v0.8+) owns deregistration + cache
# cleanup. This wrapper only handles:
#   (a) delegating to the binary when present and new enough,
#   (b) a manual-hint fallback for missing / v0.7 / broken-binary cases,
#   (c) removing the binary itself after a successful uninstall (a running
#       .exe can't delete itself on Windows — mv-aside pattern).
#
# Preserves the binary on subcommand failure so the user can retry.
#
# CLM-friendly: cmdlets over .NET static methods, native calls wrapped with
# SilentlyContinue so PS 7.4+ $PSNativeCommandUseErrorActionPreference
# doesn't kill the script on a non-zero native exit.

function Uninstall-PerfettoMcp {
    $ErrorActionPreference = 'Stop'

    $installDirOverridden = [bool]$env:INSTALL_DIR
    $installDir = if ($installDirOverridden) { $env:INSTALL_DIR } else { Join-Path $HOME '.local\bin' }
    $scope      = if ($env:SCOPE) { $env:SCOPE } else { 'user' }
    $binName    = 'perfetto-mcp-rs.exe'
    $dest       = Join-Path $installDir $binName

    function _info($m) { Write-Host "==> $m" }
    function _warn($m) { Write-Host "warning: $m" -ForegroundColor Yellow }

    # Native-command invocation wrapper: silences $ErrorActionPreference so
    # PS 7.4+ doesn't elevate a non-zero native exit to a terminating error;
    # pre-sets `$LASTEXITCODE = 1` so an exception thrown BEFORE the native
    # command runs (e.g. spawn failure) can't leave us reading a stale exit
    # code from a prior call. Block's return value comes back; caller reads
    # `$LASTEXITCODE` normally afterwards.
    function _invokeNative($block) {
        $savedEAP = $ErrorActionPreference
        $ErrorActionPreference = 'SilentlyContinue'
        $LASTEXITCODE = 1
        $result = $null
        try { $result = & $block } catch { }
        $ErrorActionPreference = $savedEAP
        $result
    }

    _info "uninstalling perfetto-mcp-rs from $installDir (scope=$scope)"
    if ($installDirOverridden) {
        _info "using INSTALL_DIR=$installDir (override). Must match the install-time value."
    }

    $removable = $false

    if (Test-Path -LiteralPath $dest) {
        # Detect v0.8+ via top-level --help output: the binary prints its
        # subcommands there; v0.7 and older don't have `uninstall`. Do NOT
        # use `uninstall --help`: clap's --help is a short-circuit arg and
        # would exit 0 on v0.7 (treating "uninstall" as a positional).
        $helpOutput = _invokeNative { (& $dest --help 2>&1 | Out-String) }
        $helpExit = $LASTEXITCODE

        if ($helpExit -eq 0) {
            if ($helpOutput -match '(?m)^\s+uninstall\b') {
                # Don't `| Out-Null` here — the binary prints step-by-step
                # outcomes (`==> Claude: deregistered...` etc.) on stdout,
                # and we want them on the user's console. _invokeNative
                # only captures $LASTEXITCODE via the EAP dance; its
                # return value is the passthrough stdout stream.
                _invokeNative { & $dest uninstall --scope $scope }
                $uninstallExit = $LASTEXITCODE

                if ($uninstallExit -eq 0) {
                    $removable = $true
                } else {
                    _warn "binary uninstall failed under --scope $scope; **keeping binary in place**"
                    _warn "for retry. Common causes: locked cache, or --scope local/project run"
                    _warn "from the wrong directory. Fix the issue and re-run uninstall.ps1."
                }
            } else {
                # v0.7 install.ps1 only ever registered --scope user (no scope
                # flag). Hardcode it in the hint regardless of the current
                # $scope — otherwise we'd hand the user a command that can't
                # reach the actual v0.7 user-scope entry.
                _warn "old binary (v0.7) without 'uninstall' subcommand; manual cleanup needed:"
                _warn "  claude mcp remove perfetto-mcp-rs --scope user"
                _warn "  codex  mcp remove perfetto-mcp-rs"
                _warn "  cache: Remove-Item -Recurse -Force `"`$env:LOCALAPPDATA\perfetto-mcp-rs`""
                $removable = $true
            }
        } else {
            _warn "installed binary exists but --help failed; **keeping it in place**"
            _warn "for inspection/retry instead of misclassifying as v0.7."
            ($helpOutput -split "`r?`n") | ForEach-Object { Write-Host "    $_" }
        }
    } else {
        # Recovery: binary already removed/moved. Best-effort deregister.
        # Cache is NOT cleaned here — the Rust binary owns that knowledge;
        # repeating `dirs::data_local_dir` rules in PowerShell is what this
        # refactor eliminates. Print manual command.
        #
        # Hardcode --scope user: can't tell whether the missing binary was
        # v0.7 (user-only) or v0.8 (any scope). user-scope is the safe
        # common denominator. Local/project entries are CWD-keyed and
        # need a manual `cd` + `claude mcp remove` anyway.
        _warn "binary missing at ${dest}; running shell fallback for deregistration."
        if (Get-Command claude -ErrorAction SilentlyContinue) {
            _invokeNative { & claude mcp remove perfetto-mcp-rs --scope user 2>&1 | ForEach-Object { Write-Host "    $_" } } | Out-Null
        }
        if (Get-Command codex -ErrorAction SilentlyContinue) {
            _invokeNative { & codex mcp remove perfetto-mcp-rs 2>&1 | ForEach-Object { Write-Host "    $_" } } | Out-Null
        }
        _warn "If you also had a --scope local/project registration, from that project dir run:"
        _warn "  claude mcp remove perfetto-mcp-rs --scope local    # or --scope project"
        _warn "Cache not auto-cleaned (binary missing). Remove manually:"
        _warn "  Remove-Item -Recurse -Force `"`$env:LOCALAPPDATA\perfetto-mcp-rs`""
        $removable = $true
    }

    if ($removable) {
        # Windows holds a DELETE lock on a running .exe; Remove-Item fails.
        # Move-Item is allowed — rename aside so the file vanishes from its
        # canonical path. Best-effort unlink of the aside; next install
        # sweeps leftovers.
        if (Test-Path -LiteralPath $dest) {
            $ts = Get-Random
            $aside = "$dest.old-$ts"
            try {
                Move-Item -LiteralPath $dest -Destination $aside -Force -ErrorAction Stop
                _info "removed $dest"
            } catch {
                _warn "could not rename $dest aside (still locked? close MCP clients and re-run)"
            }
            try { Remove-Item -LiteralPath $aside -Force -ErrorAction SilentlyContinue } catch {}
        }
        # Also sweep any leftover .old-* asides from prior installs.
        $asides = @(Get-ChildItem -LiteralPath $installDir -Filter "$binName.old-*" -Name -ErrorAction SilentlyContinue)
        foreach ($name in $asides) {
            $a = Join-Path $installDir $name
            try { Remove-Item -LiteralPath $a -Force -ErrorAction Stop } catch { _warn "could not remove $a" }
        }
    }

    # PATH note only — we don't auto-remove. `installer` wrote it on install
    # (HKCU\Environment); user can remove it manually via Environment Vars UI.
    $current = (Get-ItemProperty -Path 'HKCU:\Environment' -Name PATH -ErrorAction SilentlyContinue).PATH
    if ($null -ne $current) {
        $parts = $current -split ';' | Where-Object { $_ -ne '' }
        if ($parts -contains $installDir) {
            Write-Host ""
            Write-Host "NOTE: $installDir is still on your user PATH. The installer added it; if"
            Write-Host "you want it gone too, remove it via System Properties -> Environment"
            Write-Host "Variables (other tools may still depend on it)."
            Write-Host ""
        }
    }

    _info "done."
}

Uninstall-PerfettoMcp
