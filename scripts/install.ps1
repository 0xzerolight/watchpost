# watchpost installer (Windows)
# Usage: irm https://raw.githubusercontent.com/0xzerolight/watchpost/main/scripts/install.ps1 | iex
#
# SUPPLY-CHAIN NOTE: this downloads and runs code from a mutable branch, and
# fetches compose.prod.yml (which names the container image) from the same ref.
# A repo compromise or a MITM proxy means arbitrary code runs as you. To reduce
# that trust, read this script first, or pin a release tag:
#   $env:WATCHPOST_REF = 'v1.0.0'; irm https://raw.githubusercontent.com/0xzerolight/watchpost/v1.0.0/scripts/install.ps1 | iex
#
# No token prompt: watchpost asks for one in the browser, so a personal access
# token never has to pass through a terminal or a file.

$ErrorActionPreference = 'Stop'

$Repo   = '0xzerolight/watchpost'
$Branch = if ($env:WATCHPOST_REF)       { $env:WATCHPOST_REF }       else { 'main' }
$Dir    = if ($env:WATCHPOST_DIR)       { $env:WATCHPOST_DIR }       else { Join-Path $HOME 'watchpost' }
# The port on this machine. Deliberately not WATCHPOST_PORT: that is the port
# the binary binds inside the container, which the compose mapping fixes at
# 8080. See compose.prod.yml.
$Port   = if ($env:WATCHPOST_HOST_PORT) { $env:WATCHPOST_HOST_PORT } else { '8080' }

function Write-Info  { param($m) Write-Host "[+] $m" -ForegroundColor Green }
function Write-Warn  { param($m) Write-Host "[!] $m" -ForegroundColor Yellow }
function Write-Fail  { param($m) Write-Host "[x] $m" -ForegroundColor Red }

# --- Prerequisites ---
# The exit code is the check, not the output: a missing `docker` raises, but a
# Docker that is installed and simply not running exits non-zero with text on
# stderr, and only $LASTEXITCODE tells those apart from success.
try {
    $null = & docker compose version 2>&1
    if ($LASTEXITCODE -ne 0) { throw }
} catch {
    Write-Fail 'Docker with the Compose plugin is required but was not found.'
    Write-Host ''
    Write-Host '  Install Docker Desktop: https://www.docker.com/products/docker-desktop/'
    Write-Host '  Make sure it is running, then re-run this script.'
    exit 1
}

$composeVersion = (docker compose version 2>&1) | Select-Object -First 1
Write-Info "Docker found: $composeVersion"

# --- Install directory ---
Write-Info "Installing to $Dir"
New-Item -ItemType Directory -Force -Path (Join-Path $Dir 'data') | Out-Null

# --- Compose file ---
$composeUrl = "https://raw.githubusercontent.com/$Repo/$Branch/compose.prod.yml"
Write-Info 'Downloading docker-compose.yml...'
Invoke-WebRequest -Uri $composeUrl -OutFile (Join-Path $Dir 'docker-compose.yml') -UseBasicParsing

# No PUID/PGID here. Docker Desktop on Windows does not map host uids onto bind
# mounts the way a Linux host does, so writing them would be cargo cult — the
# image's defaults are what the volume ends up owned by either way.
#
# The port is upserted rather than written, so a re-run never truncates vars the
# user added by hand (a WATCHPOST_TZ, a token).
$envFile = Join-Path $Dir '.env'
$lines = if (Test-Path $envFile) {
    @(Get-Content $envFile | Where-Object { $_ -notmatch '^WATCHPOST_HOST_PORT=' })
} else {
    @()
}
Set-Content -Path $envFile -Value ($lines + "WATCHPOST_HOST_PORT=$Port")

# --- Pull and start ---
Push-Location $Dir
try {
    Write-Info 'Pulling the image...'
    docker compose pull
    if ($LASTEXITCODE -ne 0) {
        Write-Fail "Could not pull the image (ghcr.io/$Repo)."
        Write-Host ''
        Write-Host '  Most likely it is not publicly accessible, or ghcr.io is unreachable.'
        Write-Host '  - Check your network and that https://ghcr.io responds.'
        Write-Host '  - Maintainers: confirm the GHCR package visibility is set to Public.'
        Write-Host '  - Pin a known release: $env:WATCHPOST_REF = "<tag>" and re-run.'
        exit 1
    }

    Write-Info 'Starting watchpost...'
    docker compose up -d
    if ($LASTEXITCODE -ne 0) { Write-Fail 'docker compose up failed.'; exit 1 }

    # --- Wait for it to answer ---
    Write-Info 'Waiting for watchpost to start...'
    $up = $false
    foreach ($i in 1..30) {
        try {
            Invoke-WebRequest -Uri "http://localhost:$Port/health" -UseBasicParsing -TimeoutSec 2 | Out-Null
            $up = $true
            break
        } catch {
            Start-Sleep -Seconds 1
        }
    }
    if (-not $up) {
        Write-Warn "No answer yet on /health. Check: cd $Dir; docker compose logs"
    }
} finally {
    Pop-Location
}

# --- Done ---
Write-Host ''
Write-Info 'watchpost is running.'
Write-Host ''
Write-Host "  Open http://localhost:$Port and paste a GitHub token to finish setup."
Write-Host "  Data is stored in: $(Join-Path $Dir 'data')"
Write-Host ''
Write-Host '  Manage it with:'
Write-Host "    cd $Dir; docker compose logs -f   # Follow the log"
Write-Host "    cd $Dir; docker compose restart   # Restart"
Write-Host "    cd $Dir; docker compose down      # Stop"
Write-Host ''
Write-Host '  Update:'
Write-Host "    cd $Dir; docker compose pull; docker compose up -d"
Write-Host ''
Write-Host '  Uninstall:'
Write-Host "    cd $Dir; docker compose down      # Stop the container"
Write-Host "    Remove-Item -Recurse -Force $Dir  # Remove the install dir and its data (irreversible)"
Write-Host ''

Start-Process "http://localhost:$Port"
