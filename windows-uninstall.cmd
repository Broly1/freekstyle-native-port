@echo off
rem Double-click to run windows\uninstall.ps1 (see the top of that file).
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows\uninstall.ps1" %*
set code=%errorlevel%
echo.
pause
exit /b %code%
