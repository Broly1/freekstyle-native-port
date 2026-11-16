@echo off
rem Double-click to run windows\install.ps1 (see the top of that file).
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0windows\install.ps1" %*
set code=%errorlevel%
echo.
pause
exit /b %code%
