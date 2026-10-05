@echo off
REM MFK Run Script for Windows Command Prompt (cmd.exe)
REM Runs the kernel in VirtualBox, QEMU, or Hyper-V (UEFI-only, Generation 2)

setlocal enabledelayedexpansion

set "KERNEL_PATH=target/x86_64-mfk/debug/mfk-kernel"
set "HYPERVISOR=--vbox"
set "FIRMWARE="
set "EXTRA="

REM Parse command line arguments
:parse
if "%~1"=="" goto run_with_defaults
if "%~1"=="--qemu" set "HYPERVISOR=--qemu" & shift & goto parse
if "%~1"=="--vbox" set "HYPERVISOR=--vbox" & shift & goto parse
if "%~1"=="--virtualbox" set "HYPERVISOR=--vbox" & shift & goto parse
if "%~1"=="--hyperv" set "HYPERVISOR=--hyperv" & shift & goto parse
if "%~1"=="--hyper-v" set "HYPERVISOR=--hyperv" & shift & goto parse
if "%~1"=="--hv" set "HYPERVISOR=--hyperv" & shift & goto parse
if "%~1"=="--uefi" set "FIRMWARE=--uefi" & shift & goto parse
if "%~1"=="--bios" set "FIRMWARE=--bios" & shift & goto parse
if "%~1"=="--no-run" set "EXTRA=%EXTRA% --no-run" & shift & goto parse
if "%~1"=="--force" set "EXTRA=%EXTRA% --force" & shift & goto parse
if "%~1"=="--bundle-apps" set "EXTRA=%EXTRA% --bundle-apps" & shift & goto parse
if "%~1"=="--with-apps" set "EXTRA=%EXTRA% --with-apps" & shift & goto parse
echo "%~1" | findstr /B /C:"--hyperv-" /C:"--vhdx=" /C:"--data-disk-size=" /C:"--extra-disk" /C:"--boot-extra-disk" /C:"--kbd=" /C:"--xhci-kbd" >nul
if not errorlevel 1 set "EXTRA=%EXTRA% %~1" & shift & goto parse
set "KERNEL_PATH=%~1" & shift & goto parse

:run_with_defaults
if "%HYPERVISOR%"=="--hyperv" (
    if "%FIRMWARE%"=="--bios" (
        echo ERROR: --hyperv is UEFI-only ^(Generation 2^); drop --bios or pass --uefi. 1>&2
        exit /b 1
    )
)
echo Running MFK Kernel...
echo   Kernel: %KERNEL_PATH%
echo   Hypervisor: %HYPERVISOR% %FIRMWARE%%EXTRA%
echo.

cargo run -p mfk-runner --bin mfk-runner --release -- %KERNEL_PATH% %HYPERVISOR% %FIRMWARE%%EXTRA%
if errorlevel 1 (
    echo.
    echo ERROR: Runner failed
    exit /b 1
)
