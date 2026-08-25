# Cross-user daemon rejection acceptance check (roadmap P0 §0.1).
#
# Runs on a Windows GitHub Actions runner (admin rights are required
# to create a second local user). It starts `alex daemon` as the
# current runner user and then runs `alex status --pipe <pipe>` as a
# different local user. The daemon must reject that connection: the
# named-pipe DACL only grants LocalSystem + the daemon user, and the
# post-open Token User SID check is defence-in-depth.
#
# Exit code 0 = rejection verified (or the environment cannot create a
# second user, in which case the check is skipped so dev machines stay
# green). Non-zero = rejection FAILED.
param(
    [string]$PipeName = "alex-runtime-ci-cross-user",
    [string]$WorkDir = ".\target\cross-user-ci",
    [string]$BinDir = ".\target\debug"
)
$ErrorActionPreference = "Stop"

$principal = New-Object Security.Principal.WindowsPrincipal(
    [Security.Principal.WindowsIdentity]::GetCurrent()
)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host "SKIP: not administrator; cannot create a second local user"
    exit 0
}

$otherUser = "alexciother"
$otherPassword = "Alex-CI-other-12345!"
$pipe = "\\.\pipe\$PipeName"

New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
$exe = Join-Path $BinDir "alex.exe"
if (-not (Test-Path $exe)) {
    throw "alex.exe not found at $exe (build it before running this script)"
}

# 1. Idempotently create the second local user.
if (-not (Get-LocalUser -Name $otherUser -ErrorAction SilentlyContinue)) {
    $secure = ConvertTo-SecureString $otherPassword -AsPlainText -Force
    New-LocalUser -Name $otherUser -Password $secure -PasswordNeverExpires `
        -Description "Alex CI cross-user test account" | Out-Null
}

# 2. Start the daemon as the current (runner) user.
$state = Join-Path $WorkDir "state.json"
$installRoot = Join-Path $WorkDir "apps"
$permissionsRoot = Join-Path $WorkDir "permissions"
New-Item -ItemType Directory -Force -Path $installRoot, $permissionsRoot | Out-Null

$daemonOut = Join-Path $WorkDir "daemon.out.log"
$daemonErr = Join-Path $WorkDir "daemon.err.log"
$daemon = Start-Process -FilePath $exe `
    -ArgumentList @("daemon", "--pipe", $PipeName, "--state", $state,
                    "--install-root", $installRoot, "--permissions-root", $permissionsRoot) `
    -RedirectStandardOutput $daemonOut -RedirectStandardError $daemonErr -PassThru

try {
    # Wait for the named pipe to come up.
    $deadline = (Get-Date).AddSeconds(60)
    while (-not (Test-Path $pipe)) {
        if ((Get-Date) -gt $deadline) {
            $tail = Get-Content $daemonErr -Raw -ErrorAction SilentlyContinue
            throw "daemon pipe did not appear within 60s; stderr: $tail"
        }
        Start-Sleep -Milliseconds 500
    }

    # 3. Run `alex status` as the second local user. The daemon must
    # reject the connection before it ever reaches app lookup.
    $secure = ConvertTo-SecureString $otherPassword -AsPlainText -Force
    $cred = New-Object System.Management.Automation.PSCredential($otherUser, $secure)
    $otherOut = Join-Path $WorkDir "other.out.log"
    $otherErr = Join-Path $WorkDir "other.err.log"
    $client = Start-Process -FilePath $exe `
        -ArgumentList @("status", "com.example.crossuser", "--pipe", $PipeName) `
        -Credential $cred `
        -RedirectStandardOutput $otherOut -RedirectStandardError $otherErr `
        -PassThru -Wait

    $stderr = Get-Content $otherErr -Raw -ErrorAction SilentlyContinue
    if ($client.ExitCode -eq 0) {
        throw "second user connected successfully; cross-user rejection FAILED"
    }
    Write-Host "PASS: second user rejected (exit $($client.ExitCode)): $($stderr.Trim())"
} finally {
    if ($daemon -and -not $daemon.HasExited) {
        & $exe shutdown --pipe $PipeName 2>$null | Out-Null
        Start-Sleep -Seconds 2
        if (-not $daemon.HasExited) {
            Stop-Process -Id $daemon.Id -Force -ErrorAction SilentlyContinue
        }
    }
    Remove-LocalUser -Name $otherUser -ErrorAction SilentlyContinue
}
