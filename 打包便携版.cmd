@echo off
setlocal
powershell.exe -NoLogo -NoProfile -ExecutionPolicy Bypass -File "%~dp0scripts\Package-Portable.ps1" %*
set "package_result=%errorlevel%"
echo.
if not "%package_result%"=="0" (
    echo Packaging failed. See the error above.
) else (
    echo Package created in the dist folder.
)
pause
exit /b %package_result%
