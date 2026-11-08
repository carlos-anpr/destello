@echo off
rem Instala Destello para el usuario actual (sin administrador).
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1"
if errorlevel 1 (
    echo.
    echo No se pudo instalar Destello.
)
pause
