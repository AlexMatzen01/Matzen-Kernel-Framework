@echo off
REM MFK Build Script for Windows Command Prompt (cmd.exe)
REM Builds the kernel and runner

setlocal enabledelayedexpansion

echo Building MFK Kernel Framework...
echo.

REM Step 1: Build the kernel ( -Zjson-target-spec stabilized since cargo 1.91, removed )
REM sha2 v0.11+ (via lzma-rust2 for XZ) defaults to x86 SIMD backends that LLVM
REM cannot lower on our +soft-float/-sse target ("Do not know how to split the
REM result of this operator!"). Force the portable backend. Mirrors
REM .cargo/config.toml so direct `cargo build` invocations work too.
REM `aes_force_soft` / `polyval_force_soft` do the same for the TLS stack.
if defined RUSTFLAGS ( set "RUSTFLAGS=%RUSTFLAGS% --cfg sha2_backend="soft" --cfg aes_force_soft --cfg polyval_force_soft" ) else ( set "RUSTFLAGS=--cfg sha2_backend="soft" --cfg aes_force_soft --cfg polyval_force_soft" )
REM TLS 1.3 (`net_tls`) is in the kernel's default features. Pass --no-tls to
REM build without it (falls back to tls_stub); --tls is a no-op.
set "MFK_FEATURES="
for %%A in (%*) do if "%%A"=="--no-tls" set "MFK_FEATURES=--no-default-features --features usb"
echo [1/2] Building kernel...
cargo build -p mfk-kernel --target targets/x86_64-mfk.json -Zbuild-std=core,alloc -Zbuild-std-features=compiler-builtins-mem %MFK_FEATURES%
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
