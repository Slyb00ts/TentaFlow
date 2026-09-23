@echo off
REM ============================================================================
REM scripts\setup.bat — wrapper dla setup.ps1, zeby dzialalo z cmd.exe.
REM
REM Uzycie:
REM   scripts\setup                # baza + CUDA (gdy jest GPU NVIDIA) + Vulkan SDK
REM   scripts\setup -NoCuda
REM   scripts\setup -Minimal       # bez GPU SDK
REM   scripts\setup -Help
REM ============================================================================
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0setup.ps1" %*
exit /b %ERRORLEVEL%
