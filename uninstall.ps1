# Uninstall perfetto-mcp-rs and deregister it from Claude Code / Codex.
#
# Usage:
#   irm https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/uninstall.ps1 | iex
#
# Environment overrides:
#   $env:INSTALL_DIR   Where the binary lives (default: $HOME\.local\bin)
#
# Idempotent: missing CLI / missing binary / empty cache dir are silent
# no-ops. claude/codex `mcp remove` failures (including "never registered")
# are surfaced with their raw output so users can distinguish benign cases
# from real config errors. PATH is intentionally not modified.
#
# Mirrors install.ps1's CLM-friendly conventions: cmdlets over .NET static
# methods where possible, and native CLI calls wrapped with
# SilentlyContinue so PS 7.4+ $PSNativeCommandUseErrorActionPreference
# doesn't kill the script on a non-zero exit.

function Uninstall-PerfettoMcp {
    $ErrorActionPreference = 'Stop'

    $installDirOverridden = [bool]$env:INSTALL_DIR
    $installDir = if ($installDirOverridden) { $env:INSTALL_DIR } else { Join-Path $HOME '.local\bin' }
    $binName    = 'perfetto-mcp-rs.exe'
    $dest       = Join-Path $installDir $binName

    function _info($m) { Write-Host "==> $m" }
    function _warn($m) { Write-Host "warning: $m" -ForegroundColor Yellow }

    _info "uninstalling perfetto-mcp-rs from $installDir"
    if ($installDirOverridden) {
        _info "using INSTALL_DIR=$installDir (override). If you customized `$env:INSTALL_DIR at install time, make sure this matches."
    }

    # --- Claude Code ---
    if (-not (Get-Command claude -ErrorAction SilentlyContinue)) {
        Write-Host ""
        Write-Host "NOTE: 'claude' CLI not found. If you registered the server with Claude Code"
        Write-Host "elsewhere, run:"
        Write-Host ""
        Write-Host "    claude mcp remove perfetto-mcp-rs --scope user"
        Write-Host ""
    } else {
        # We can't tell "server never registered" apart from "config broken" by exit
        # code alone, so capture output and surface it on non-zero. Letting the user
        # judge beats silently lying about success.
        $savedEAP = $ErrorActionPreference
        $ErrorActionPreference = 'SilentlyContinue'
        # Initialize before try so an exception (e.g. process spawn failure
        # between Get-Command probe and execution) doesn't leave us reading
        # a stale $LASTEXITCODE from a prior native call.
        $claudeOutput = ''
        $LASTEXITCODE = 1
        try { $claudeOutput = (& claude mcp remove perfetto-mcp-rs --scope user 2>&1 | Out-String).TrimEnd() } catch { $claudeOutput = "$_"; $LASTEXITCODE = 1 }
        $claudeRemoveExit = $LASTEXITCODE
        $ErrorActionPreference = $savedEAP

        if ($claudeRemoveExit -eq 0) {
            _info "deregistered from Claude Code (user scope). Restart Claude Code to drop it."
        } else {
            _warn "claude mcp remove exited $claudeRemoveExit. Output:"
            ($claudeOutput -split "`r?`n") | ForEach-Object { Write-Host "    $_" }
            _warn "Safe to ignore if perfetto-mcp-rs was never registered. Otherwise verify with: claude mcp list"
        }
    }

    # --- Codex ---
    if (-not (Get-Command codex -ErrorAction SilentlyContinue)) {
        Write-Host ""
        Write-Host "NOTE: 'codex' CLI not found. If you registered the server with Codex"
        Write-Host "elsewhere, run:"
        Write-Host ""
        Write-Host "    codex mcp remove perfetto-mcp-rs"
        Write-Host ""
    } else {
        $savedEAP = $ErrorActionPreference
        $ErrorActionPreference = 'SilentlyContinue'
        $codexOutput = ''
        $LASTEXITCODE = 1
        try { $codexOutput = (& codex mcp remove perfetto-mcp-rs 2>&1 | Out-String).TrimEnd() } catch { $codexOutput = "$_"; $LASTEXITCODE = 1 }
        $codexRemoveExit = $LASTEXITCODE
        $ErrorActionPreference = $savedEAP

        if ($codexRemoveExit -eq 0) {
            _info "deregistered from Codex. New Codex sessions will drop it."
        } else {
            _warn "codex mcp remove exited $codexRemoveExit. Output:"
            ($codexOutput -split "`r?`n") | ForEach-Object { Write-Host "    $_" }
            _warn "Safe to ignore if perfetto-mcp-rs was never registered. Otherwise verify with: codex mcp list"
        }
    }

    # --- Binary ---
    if (Test-Path -LiteralPath $dest) {
        try {
            Remove-Item -LiteralPath $dest -Force -ErrorAction Stop
            _info "removed $dest"
        } catch {
            _warn "could not remove $dest - is an MCP client still running it? Close Claude Code, Codex, or any other client using it and retry."
        }
    } else {
        _info "no binary at $dest (already removed or never installed)."
    }

    # --- Cache directory (matches src/download.rs cache_dir()) ---
    $cacheBase = $env:LOCALAPPDATA
    if ($cacheBase) {
        $cacheDir = Join-Path $cacheBase 'perfetto-mcp-rs'
        if (Test-Path -LiteralPath $cacheDir) {
            try {
                Remove-Item -LiteralPath $cacheDir -Recurse -Force -ErrorAction Stop
                _info "removed cache $cacheDir"
            } catch {
                _warn "could not remove cache ${cacheDir}: $($_.Exception.Message)"
            }
        } else {
            _info "no cache at $cacheDir (already removed or never downloaded)."
        }
    }

    # --- Aside files from prior installs ---
    $asides = @(Get-ChildItem -LiteralPath $installDir -Filter "$binName.old-*" -Name -ErrorAction SilentlyContinue)
    foreach ($name in $asides) {
        $aside = Join-Path $installDir $name
        try {
            Remove-Item -LiteralPath $aside -Force -ErrorAction Stop
            _info "removed aside $aside"
        } catch {
            _warn "could not remove $aside (still locked?)"
        }
    }

    # --- PATH note (no automatic edit) ---
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
