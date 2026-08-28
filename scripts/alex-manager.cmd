@echo off
setlocal

set "ALEX_DIST=%~dp0"
set "ALEX_BIN=%ALEX_DIST%alex.exe"
set "ALEX_MANAGER_PACKAGE=%ALEX_DIST%manager.alex"
set "ALEX_INSTALL_ROOT=%LOCALAPPDATA%\AlexRuntime\apps"

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

"%ALEX_BIN%" manager --install-root "%ALEX_INSTALL_ROOT%"
if errorlevel 1 (
  echo Alex Manager exited with an error.
  pause
  exit /b 1
)

