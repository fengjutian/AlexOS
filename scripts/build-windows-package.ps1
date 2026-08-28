[CmdletBinding()]
param(
    [string]$OutputRoot,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $OutputRoot) {
    $OutputRoot = Join-Path $repoRoot 'target\release-package'
}
$OutputRoot = [System.IO.Path]::GetFullPath($OutputRoot)

$cargoManifest = Get-Content (Join-Path $repoRoot 'Cargo.toml') -Raw
$versionMatch = [regex]::Match($cargoManifest, '(?m)^version\s*=\s*"([^"]+)"')
if (-not $versionMatch.Success) { throw 'Could not read the package version from Cargo.toml' }
$version = $versionMatch.Groups[1].Value
$packageName = "alex-runtime-$version-windows-x64"
$packageDir = Join-Path $OutputRoot $packageName
$archivePath = Join-Path $OutputRoot "$packageName.zip"
$alex = Join-Path $repoRoot 'target\release\alex.exe'
$managerArchive = Join-Path $OutputRoot 'manager.alex'

if (-not $SkipBuild) {
    Push-Location $repoRoot
    try {
        cargo build --release --locked
        if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    } finally { Pop-Location }
}
if (-not (Test-Path -LiteralPath $alex -PathType Leaf)) {
    throw "Release executable not found: $alex"
}

New-Item -ItemType Directory -Path $OutputRoot -Force | Out-Null
if (Test-Path -LiteralPath $managerArchive) { Remove-Item -LiteralPath $managerArchive -Force }
& $alex pack (Join-Path $repoRoot 'plugins\manager') $managerArchive
if ($LASTEXITCODE -ne 0) { throw 'Manager package creation failed' }

if (Test-Path -LiteralPath $packageDir) { Remove-Item -LiteralPath $packageDir -Recurse -Force }
New-Item -ItemType Directory -Path $packageDir | Out-Null
Copy-Item -LiteralPath $alex -Destination (Join-Path $packageDir 'alex.exe')
Copy-Item -LiteralPath $managerArchive -Destination (Join-Path $packageDir 'manager.alex')
Copy-Item -LiteralPath (Join-Path $repoRoot 'scripts\alex-manager.cmd') -Destination (Join-Path $packageDir 'Alex Manager.cmd')
Copy-Item -LiteralPath (Join-Path $repoRoot 'LICENSE') -Destination $packageDir

$commit = try { (git -C $repoRoot rev-parse HEAD).Trim() } catch { 'unknown' }
$builtAt = [DateTime]::UtcNow.ToString('o')
$artifacts = Get-ChildItem -LiteralPath $packageDir -File | ForEach-Object {
    [ordered]@{
        file = $_.Name
        sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        bytes = $_.Length
    }
}
$manifest = [ordered]@{
    schemaVersion = 1
    product = 'Alex Runtime'
    version = $version
    channel = 'developer-preview'
    target = 'windows-x86_64'
    commit = $commit
    builtAt = $builtAt
    artifacts = @($artifacts)
}
$manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $packageDir 'release-manifest.json') -Encoding utf8

$checksums = Get-ChildItem -LiteralPath $packageDir -File | Sort-Object Name | ForEach-Object {
    $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $($_.Name)"
}
$checksums | Set-Content -LiteralPath (Join-Path $packageDir 'checksums.txt') -Encoding ascii

if (Test-Path -LiteralPath $archivePath) { Remove-Item -LiteralPath $archivePath -Force }
Compress-Archive -LiteralPath $packageDir -DestinationPath $archivePath -CompressionLevel Optimal
Write-Host "Windows package: $archivePath" -ForegroundColor Green

