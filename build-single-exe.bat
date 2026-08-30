@echo off
setlocal
cd /d "%~dp0"
echo ========================================
echo Smart Kernel Brain - Single EXE build
echo ========================================
where cargo >nul 2>nul
if errorlevel 1 (
  echo.
  echo [ERROR] Rust/Cargo was not found in PATH.
  echo Install Rust, reopen this terminal, and run this file again.
  echo Nothing was installed or deleted.
  echo.
  pause
  exit /b 1
)
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-single-exe.ps1"
set "RC=%ERRORLEVEL%"
echo.
if not "%RC%"=="0" (
  echo [ERROR] Build failed with exit code %RC%.
) else (
  echo [OK] Build completed. See dist\SKB.exe
)
echo.
pause
exit /b %RC%
