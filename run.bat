@echo off
REM MFK Run Script for Windows Command Prompt (cmd.exe)
REM Runs the kernel in VirtualBox or QEMU

setlocal enabledelayedexpansion

set "KERNEL_PATH=target/x86_64-mfk/debug/mfk-kernel"
set "HYPERVISOR=--vbox"

REM Parse command line arguments
if "%~1"=="" goto run_with_defaults
if "%~1"=="--qemu" goto qemu_mode
if "%~1"=="--vbox" goto vbox_mode
if "%~1"=="--virtualbox" goto vbox_mode
set "KERNEL_PATH=%~1"

:check_second_arg
if "%~2"=="" goto run_with_defaults
if "%~2"=="--qemu" set "HYPERVISOR=--qemu" & goto run_with_defaults
if "%~2"=="--vbox" set "HYPERVISOR=--vbox" & goto run_with_defaults
if "%~2"=="--virtualbox" set "HYPERVISOR=--vbox" & goto run_with_defaults

:qemu_mode
set "HYPERVISOR=--qemu"
goto run_with_defaults

:vbox_mode
set "HYPERVISOR=--vbox"

:run_with_defaults
echo Running MFK Kernel...
echo   Kernel: %KERNEL_PATH%
echo   Hypervisor: %HYPERVISOR%
echo.

cargo run -p mfk-runner --release -- %KERNEL_PATH% %HYPERVISOR%
if errorlevel 1 (
    echo.
    echo ERROR: Runner failed
    exit /b 1
)
