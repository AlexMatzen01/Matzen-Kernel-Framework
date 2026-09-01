@echo off
REM MFK Build Script for Windows Command Prompt (cmd.exe)
REM Builds the kernel and runner

setlocal enabledelayedexpansion

echo Building MFK Kernel Framework...
echo.

REM Step 1: Build the kernel ( -Zjson-target-spec stabilized since cargo 1.91, removed )
echo [1/2] Building kernel...
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem
if errorlevel 1 (
    echo ERROR: Kernel build failed
    exit /b 1
)
echo Build kernel complete
echo.

REM Step 2: Build the runner
echo [2/2] Building runner...
cargo build -p mfk-runner --release
if errorlevel 1 (
    echo ERROR: Runner build failed
    exit /b 1
)
echo Build runner complete
echo.

echo ======================================
echo Build Complete!
echo ======================================
echo.
echo Next, run the kernel:
echo   run.bat
echo.
