@echo off
REM ============================================================================
REM scripts\build.bat — wrapper dla build.ps1, zeby dzialalo z cmd.exe.
REM cmd.exe ma .ps1 association = notepad, wiec nie odpala skryptu.
REM Ten plik forwarduje wszystkie argumenty do PowerShella.
REM
REM Uzycie:
REM   scripts\build                              # cargo build
REM   scripts\build --features gpu-vulkan
REM   scripts\build --release --features gpu-vulkan
REM   scripts\build -Cmd run --features gpu-vulkan
REM ============================================================================
powershell.exe -ExecutionPolicy Bypass -File "%~dp0build.ps1" %*
exit /b %ERRORLEVEL%
