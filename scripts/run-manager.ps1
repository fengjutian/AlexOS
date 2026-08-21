# Runs the self-hosted Alex OS Manager: builds release, packs the
# `com.alex.manager` plugin, installs it, and launches `alex manager`.
#
# Usage (from the repo root):
#   .\scripts\run-manager.ps1
#
# Optional environment overrides:
#   $env:ALEX_DEVTOOLS=1   enable WebView2 DevTools (F12) for debugging
#   $env:ALEX_INSTALL_ROOT override the install root (default: target\apps)

$ErrorActionPreference = 'Stop'

$repoRoot = (Resolve-Path "$PSScriptRoot\..").Path
Set-Location $repoRoot

$alex = Join-Path $repoRoot "target\release\alex.exe"
$installRoot = if ($env:ALEX_INSTALL_ROOT) { $env:ALEX_INSTALL_ROOT } else { Join-Path $repoRoot "target\apps" }
$pluginDir = Join-Path $repoRoot "plugins\manager"
$archive = Join-Path $repoRoot "target\manager.alex"

# 1. Make sure release binary is up to date.
Write-Host "==> cargo build --release" -ForegroundColor Cyan
cargo build --release --offline
if ($LASTEXITCODE -ne 0) { throw "cargo build failed" }

# 2. Make sure install root exists.
if (-not (Test-Path $installRoot)) {
    New-Item -ItemType Directory -Path $installRoot -Force | Out-Null
}

# 3. Remove the old .alex so pack does not fail with "already installed".
if (Test-Path $archive) {
    Write-Host "==> removing old $archive" -ForegroundColor DarkGray
    Remove-Item $archive -Force
}

# 4. Uninstall any pre-existing com.alex.manager so install will not
#    trip the "already installed" guard.
$installedDir = Join-Path $installRoot "com.alex.manager"
if (Test-Path $installedDir) {
    Write-Host "==> uninstall previous com.alex.manager" -ForegroundColor DarkGray
    & $alex uninstall com.alex.manager --root $installRoot
    if ($LASTEXITCODE -ne 0) { throw "uninstall failed" }
}

# 5. Pack and install.
Write-Host "==> pack + install com.alex.manager" -ForegroundColor Cyan
& $alex pack $pluginDir $archive
if ($LASTEXITCODE -ne 0) { throw "pack failed" }

& $alex install $archive --root $installRoot
if ($LASTEXITCODE -ne 0) { throw "install failed" }

# 6. Launch. This blocks until the WebView window is closed.
Write-Host "==> launching alex manager (close the WebView window to exit)" -ForegroundColor Green
& $alex manager --install-root $installRoot
