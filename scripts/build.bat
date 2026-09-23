@echo off
REM ============================================================================
REM scripts\build.bat — wrapper dla build.ps1, zeby dzialalo z cmd.exe.
REM cmd.exe ma .ps1 association = notepad, wiec nie odpala skryptu.
REM Ten plik forwarduje wszystkie argumenty do PowerShella.
REM
REM Uzycie:
REM   scripts\build -Edition slim --release
REM   scripts\build -Edition full -Backend cuda --release
REM   scripts\build -Edition full -Backend vulkan --release
REM   scripts\build -Cmd test -p tentaflow-core --lib
REM ============================================================================
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0build.ps1" %*
exit /b %ERRORLEVEL%
