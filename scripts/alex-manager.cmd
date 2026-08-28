@echo off
setlocal

set "ALEX_DIST=%~dp0"
set "ALEX_BIN=%ALEX_DIST%alex.exe"
set "ALEX_MANAGER_PACKAGE=%ALEX_DIST%manager.alex"
set "ALEX_INSTALL_ROOT=%LOCALAPPDATA%\AlexRuntime\apps"
set "ALEX_DAEMON_ROOT=%LOCALAPPDATA%\AlexRuntime\daemon"
set "ALEX_PIPE=\\.\pipe\alex-runtime-v1"

if not exist "%ALEX_BIN%" (
  echo Alex Runtime executable is missing: "%ALEX_BIN%"
  pause
  exit /b 1
)

if not exist "%ALEX_MANAGER_PACKAGE%" (
  echo Alex Manager package is missing: "%ALEX_MANAGER_PACKAGE%"
  pause
  exit /b 1
)

if not exist "%ALEX_INSTALL_ROOT%\com.alex.manager\manifest.json" (
  echo Installing Alex Manager for this user...
  "%ALEX_BIN%" install "%ALEX_MANAGER_PACKAGE%" --root "%ALEX_INSTALL_ROOT%"
  if errorlevel 1 (
    echo Alex Manager installation failed.
    pause
    exit /b 1
  )
)

if not exist "%ALEX_DAEMON_ROOT%" mkdir "%ALEX_DAEMON_ROOT%"

powershell.exe -NoProfile -Command "if (Test-Path -LiteralPath '%ALEX_PIPE%') { exit 0 } else { exit 1 }"
if errorlevel 1 (
  echo Starting Alex Runtime...
  start "Alex Runtime Daemon" /b "%ALEX_BIN%" daemon --state "%ALEX_DAEMON_ROOT%\state.json" --pipe "%ALEX_PIPE%" --install-root "%ALEX_INSTALL_ROOT%" --permissions-root "%ALEX_DAEMON_ROOT%\permissions" 1>"%ALEX_DAEMON_ROOT%\stdout.log" 2>"%ALEX_DAEMON_ROOT%\stderr.log"
  powershell.exe -NoProfile -Command "$deadline=[DateTime]::UtcNow.AddSeconds(15); while (-not (Test-Path -LiteralPath '%ALEX_PIPE%')) { if ([DateTime]::UtcNow -ge $deadline) { exit 1 }; Start-Sleep -Milliseconds 100 }"
  if errorlevel 1 (
    echo Alex Runtime did not start. See "%ALEX_DAEMON_ROOT%\stderr.log".
    pause
    exit /b 1
  )
)

"%ALEX_BIN%" manager --install-root "%ALEX_INSTALL_ROOT%" --pipe "%ALEX_PIPE%"
if errorlevel 1 (
  echo Alex Manager exited with an error.
  pause
  exit /b 1
)
