@echo off
REM ============================================================================
REM scripts\setup.bat — wrapper dla setup.ps1, zeby dzialalo z cmd.exe.
REM
REM Uzycie:
REM   scripts\setup                # tylko bazowe zaleznosci
REM   scripts\setup -Vulkan
REM   scripts\setup -AllGpu        # CUDA + Vulkan + ROCm (jako Admin dla ROCm)
REM   scripts\setup -Help
REM ============================================================================
powershell.exe -ExecutionPolicy Bypass -File "%~dp0setup.ps1" %*
exit /b %ERRORLEVEL%
