[CmdletBinding()]
param(
    [string]$OutputRoot,
    [switch]$SkipBuild,
    [string]$SigningThumbprint,
    [string]$TimestampUrl = 'http://timestamp.digicert.com'
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $OutputRoot) {
    $OutputRoot = Join-Path $repoRoot 'target\release-installer'
}
$OutputRoot = [System.IO.Path]::GetFullPath($OutputRoot)
$packageOutput = Join-Path $repoRoot 'target\release-package'

if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot 'build-windows-package.ps1') -OutputRoot $packageOutput
    if ($LASTEXITCODE -ne 0) { throw 'Windows package build failed' }
}

$cargoManifest = Get-Content (Join-Path $repoRoot 'Cargo.toml') -Raw
$versionMatch = [regex]::Match($cargoManifest, '(?m)^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) { throw 'Could not read package version from Cargo.toml' }
$version = $versionMatch.Groups[1].Value
$packageName = "alex-runtime-$version-windows-x64"
$packageDir = Join-Path $packageOutput $packageName
if (-not (Test-Path -LiteralPath $packageDir -PathType Container)) {
    throw "Portable package directory not found: $packageDir"
}

$wix = Get-Command wix.exe -ErrorAction SilentlyContinue
if (-not $wix) { $wix = Get-Command wix -ErrorAction SilentlyContinue }
if (-not $wix) {
    throw 'WiX Toolset v4 is required. Install it with: dotnet tool install --global wix'
}

$signtool = $null
if ($SigningThumbprint) {
    $signtool = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if (-not $signtool) {
        throw 'signtool.exe is required when -SigningThumbprint is supplied (install the Windows SDK)'
    }
}

New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
$stage = Join-Path $OutputRoot $packageName
if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
Copy-Item -LiteralPath $packageDir -Destination $stage -Recurse

function Invoke-AuthenticodeSign([string]$Path) {
    if (-not $SigningThumbprint) { return }
    & $signtool.Source sign /sha1 $SigningThumbprint /fd SHA256 /tr $TimestampUrl /td SHA256 $Path
    if ($LASTEXITCODE -ne 0) { throw "Authenticode signing failed: $Path" }
    & $signtool.Source verify /pa /all $Path
    if ($LASTEXITCODE -ne 0) { throw "Authenticode verification failed: $Path" }
}

Invoke-AuthenticodeSign (Join-Path $stage 'alex.exe')

# Signing changes alex.exe bytes. Rebuild the staged manifest and checksums so
# the MSI never ships stale integrity metadata copied from the portable ZIP.
$releaseManifestPath = Join-Path $stage 'release-manifest.json'
$releaseManifest = Get-Content -LiteralPath $releaseManifestPath -Raw | ConvertFrom-Json
$releaseManifest.artifacts = @(
    Get-ChildItem -LiteralPath $stage -File |
        Where-Object { $_.Name -notin @('release-manifest.json', 'checksums.txt') } |
        ForEach-Object {
            [ordered]@{
                file = $_.Name
                sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
                bytes = $_.Length
            }
        }
)
$releaseManifest | ConvertTo-Json -Depth 5 |
    Set-Content -LiteralPath $releaseManifestPath -Encoding utf8
$stageChecksums = Get-ChildItem -LiteralPath $stage -File |
    Where-Object Name -ne 'checksums.txt' |
    Sort-Object Name |
    ForEach-Object {
        $fileHash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        "$fileHash  $($_.Name)"
    }
$stageChecksums | Set-Content -LiteralPath (Join-Path $stage 'checksums.txt') -Encoding ascii

$installer = Join-Path $OutputRoot "alex-runtime-$version-windows-x64.msi"
if (Test-Path -LiteralPath $installer) { Remove-Item -LiteralPath $installer -Force }
$source = Join-Path $repoRoot 'installer\windows\AlexRuntime.wxs'
& $wix.Source build $source -arch x64 "-dVersion=$version" "-dPackageDir=$stage" -o $installer
if ($LASTEXITCODE -ne 0) { throw 'WiX MSI build failed' }

Invoke-AuthenticodeSign $installer
$hash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $([System.IO.Path]::GetFileName($installer))" |
    Set-Content -LiteralPath (Join-Path $OutputRoot 'installer-checksums.txt') -Encoding ascii

Write-Host "Windows installer: $installer" -ForegroundColor Green
if (-not $SigningThumbprint) {
    Write-Warning 'The MSI and alex.exe are unsigned developer artifacts. Supply -SigningThumbprint for release signing.'
}
