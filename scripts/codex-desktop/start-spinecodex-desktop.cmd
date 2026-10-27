@echo off
setlocal EnableExtensions

set "LAUNCHER=%~dp0start-spinecodex-desktop.ps1"
if not exist "%LAUNCHER%" (
    echo Missing launcher script: "%LAUNCHER%"
    pause
    exit /b 1
)

powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "%LAUNCHER%"
exit /b %ERRORLEVEL%
