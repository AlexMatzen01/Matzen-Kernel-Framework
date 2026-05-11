@echo off
REM MFK Installation Script for Windows Command Prompt (cmd.exe)
REM Sets up Rust nightly and required components for building the kernel

setlocal enabledelayedexpansion

echo ======================================
echo MFK Installation Setup (Windows)
echo ======================================
echo.

REM Step 1: Check if Rust is installed
echo [1/3] Checking Rust installation...
rustc --version >nul 2>&1
if errorlevel 1 (
    echo ERROR: Rust not found
    echo.
    echo Please install Rust from: https://rustup.rs/
    echo.
    echo Or use Chocolatey:
    echo   choco install rust
    echo.
    exit /b 1
)

for /f "tokens=*" %%i in ('rustc --version') do set "RUST_VERSION=%%i"
echo OK - Rust installed: %RUST_VERSION%
echo.

REM Step 2: Install nightly toolchain
echo [2/3] Installing/updating nightly toolchain...
rustup toolchain install nightly >nul 2>&1
if errorlevel 1 (
    echo ERROR: Failed to install nightly toolchain
    exit /b 1
)
echo OK - Nightly toolchain installed
echo.

REM Step 3: Add required components
echo [3/3] Adding required components...
echo   Adding rust-src...
rustup component add rust-src --toolchain nightly >nul 2>&1
if errorlevel 1 (
    echo ERROR: Failed to add rust-src
    exit /b 1
)
echo   OK - rust-src added

echo   Adding llvm-tools-preview...
rustup component add llvm-tools-preview --toolchain nightly >nul 2>&1
if errorlevel 1 (
    echo ERROR: Failed to add llvm-tools-preview
    exit /b 1
)
echo   OK - llvm-tools-preview added
echo.

REM Step 4: Verify VirtualBox
echo [Bonus] Checking VirtualBox installation...
VBoxManage --version >nul 2>&1
if errorlevel 1 (
    echo WARNING: VirtualBox not found (recommended for better networking)
    echo   Install from: https://www.virtualbox.org/wiki/Downloads
    echo   Or use Chocolatey:
    echo     choco install virtualbox
) else (
    for /f "tokens=*" %%i in ('VBoxManage --version') do set "VBOX_VERSION=%%i"
    echo OK - VirtualBox found: %VBOX_VERSION%
)
echo.

echo ======================================
echo Setup Complete!
echo ======================================
echo.
echo Next steps:
echo 1. Build the project:
echo    build.bat
echo.
echo 2. Run the kernel:
echo    run.bat
echo.
