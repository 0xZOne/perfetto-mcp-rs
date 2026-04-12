# Install perfetto-mcp-rs and register it with Claude Code.
#
# Usage:
#   irm https://raw.githubusercontent.com/0xZOne/perfetto-mcp-rs/main/install.ps1 | iex
#
# Environment overrides:
#   $env:INSTALL_DIR   Where to place the binary (default: $HOME\.local\bin)
#   $env:REPO          GitHub slug to download from (default: 0xZOne/perfetto-mcp-rs)
#   $env:VERSION       Release tag to install (default: latest)

function Install-PerfettoMcp {
    $ErrorActionPreference = 'Stop'
    # Old Windows defaults (TLS 1.0) can't talk to github.com; force TLS 1.2.
    try { [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 } catch {}

    $repo       = if ($env:REPO)        { $env:REPO }        else { '0xZOne/perfetto-mcp-rs' }
    $installDir = if ($env:INSTALL_DIR) { $env:INSTALL_DIR } else { Join-Path $HOME '.local\bin' }
    $version    = if ($env:VERSION)     { $env:VERSION }     else { 'latest' }
    $asset      = 'perfetto-mcp-rs-windows-amd64.exe'
    $binName    = 'perfetto-mcp-rs.exe'

    function _info($m) { Write-Host "==> $m" }
    function _warn($m) { Write-Host "warning: $m" -ForegroundColor Yellow }
    function _fail($m) { Write-Host "error: $m" -ForegroundColor Red; throw $m }

    # Detect real OS architecture even when running under WOW64 (32-bit PS on
    # 64-bit Windows reports 'x86' in PROCESSOR_ARCHITECTURE).
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($arch -ne 'AMD64') {
        _fail "only windows-amd64 is released; detected '$arch'"
    }

    # `releases/latest/download/<asset>` redirects to the latest release asset
    # without an API call, which dodges the GitHub anonymous rate limit.
    $url = if ($version -eq 'latest') {
        "https://github.com/$repo/releases/latest/download/$asset"
    } else {
        "https://github.com/$repo/releases/download/$version/$asset"
    }

    _info "installing perfetto-mcp-rs ($version) from $repo"

    New-Item -ItemType Directory -Force -Path $installDir | Out-Null
    $dest = Join-Path $installDir $binName

    # Best-effort sweep of aside files left by prior installs. They can only
    # be deleted once the old claude subprocess holding them has exited.
    Get-ChildItem -LiteralPath $installDir -Filter "$binName.old-*" -ErrorAction SilentlyContinue |
        ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }

    # Windows holds a DELETE lock on a running .exe, so we can't overwrite or
    # unlink it — but MoveFile is allowed on locked files. Rename aside so
    # the download target is free; the aside is cleaned up next install.
    if (Test-Path -LiteralPath $dest) {
        $aside = "$dest.old-$([DateTimeOffset]::Now.ToUnixTimeSeconds())"
        try {
            Move-Item -LiteralPath $dest -Destination $aside -Force -ErrorAction Stop
        } catch {
            _fail "cannot replace $dest - is Claude Code running with it? Close Claude Code and retry."
        }
    }

    try {
        Invoke-WebRequest -Uri $url -OutFile $dest -UseBasicParsing
    } catch {
        _fail "download failed: $url ($($_.Exception.Message))"
    }
    _info "installed to $dest"

    # Idempotent user-PATH update. SetEnvironmentVariable(User) writes
    # HKCU\Environment and broadcasts WM_SETTINGCHANGE, so new processes pick
    # it up without logout; the current session still needs to reload.
    $current = [Environment]::GetEnvironmentVariable('PATH', 'User')
    if ($null -eq $current) { $current = '' }
    $parts = $current -split ';' | Where-Object { $_ -ne '' }
    if ($parts -contains $installDir) {
        _info "$installDir is already on your user PATH"
    } else {
        $new = if ($current) { "$current;$installDir" } else { $installDir }
        [Environment]::SetEnvironmentVariable('PATH', $new, 'User')
        _info "added $installDir to your user PATH (new terminals will see it)"
    }

    # Forward-slash form avoids backslash-escaping hazards in JSON configs.
    $claudePath = ($dest -replace '\\', '/')

    if (-not (Get-Command claude -ErrorAction SilentlyContinue)) {
        Write-Host ""
        Write-Host "NOTE: 'claude' CLI not found. To use this server with Claude Code, install"
        Write-Host "Claude Code first, then run:"
        Write-Host ""
        Write-Host "    claude mcp add perfetto-mcp-rs --scope user $claudePath"
        Write-Host ""
        return
    }

    & claude mcp remove perfetto-mcp-rs --scope user 2>$null | Out-Null
    & claude mcp add perfetto-mcp-rs --scope user $claudePath 2>$null | Out-Null
    if ($LASTEXITCODE -eq 0) {
        _info "registered with Claude Code (user scope). Restart Claude Code to pick it up."
    } else {
        _warn "failed to register with Claude Code. Run manually:"
        Write-Host "    claude mcp add perfetto-mcp-rs --scope user $claudePath"
    }
}

Install-PerfettoMcp
