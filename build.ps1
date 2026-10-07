param(
    [switch]$Release,
    [switch]$Debug,
    [switch]$NoTls,
    [switch]$Tls
)

if ($Release -and $Debug) {
    Write-Error "Use only one of -Release or -Debug."
    exit 1
}

$repoRoot = $PSScriptRoot
Set-Location -LiteralPath $repoRoot

Write-Host "Building MFK Kernel Framework..." -ForegroundColor Cyan
Write-Host ""

$targetSpec = "targets/x86_64-mfk.json"
if (-not (Test-Path -LiteralPath $targetSpec)) {
    Write-Error "Target spec not found at $targetSpec"
    exit 1
}

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "cargo not found. Install Rust from https://rustup.rs/."
    exit 1
}

$buildMode = if ($Release) { "release" } else { "debug" }
$buildFlag = if ($Release) { @("--release") } else { @() }

# TLS 1.3 (`net_tls`) is in the kernel's default features. -NoTls builds
# without it (falls back to tls_stub); -Tls is a no-op (documents intent).
$featureArgs = if ($NoTls) { @("--no-default-features", "--features", "usb") } else { @() }

Write-Host "[1/2] Building kernel ($buildMode)..." -ForegroundColor Yellow
# sha2 v0.11+ (via lzma-rust2 for XZ) defaults to x86 SIMD backends that LLVM
# cannot lower on our +soft-float/-sse target ("Do not know how to split the
# result of this operator!"). Force the portable backend. Mirrors
# .cargo/config.toml so direct `cargo build` invocations work too.
# `aes_force_soft` / `polyval_force_soft` do the same for the TLS stack.
if ($env:RUSTFLAGS) { $env:RUSTFLAGS += ' --cfg sha2_backend="soft" --cfg aes_force_soft --cfg polyval_force_soft' } else { $env:RUSTFLAGS = '--cfg sha2_backend="soft" --cfg aes_force_soft --cfg polyval_force_soft' }
$kernelCmd = @(
    "build",
    "-p", "mfk-kernel",
    "--target", $targetSpec,
    "-Zbuild-std=core,alloc",
    "-Zbuild-std-features=compiler-builtins-mem"
) + $buildFlag + $featureArgs

& cargo @kernelCmd
if ($LASTEXITCODE -ne 0) {
    Write-Host "Retrying with -Zjson-target-spec for older cargo..." -ForegroundColor Yellow
    $legacyKernelCmd = @(
        "build",
        "-p", "mfk-kernel",
        "--target", $targetSpec,
        "-Zjson-target-spec",
        "-Zbuild-std=core,alloc",
        "-Zbuild-std-features=compiler-builtins-mem"
    ) + $buildFlag + $featureArgs
    & cargo @legacyKernelCmd
    if ($LASTEXITCODE -ne 0) {
        Write-Host "ERROR: Kernel build failed" -ForegroundColor Red
        exit 1
    }
}

$kernelBin = if ($Release) {
    "target/x86_64-mfk/release/mfk-kernel"
} else {
    "target/x86_64-mfk/debug/mfk-kernel"
}
if (-not (Test-Path -LiteralPath $kernelBin)) {
    Write-Error "Expected kernel binary not found at $kernelBin"
    exit 1
}
$kernelSize = (Get-Item -LiteralPath $kernelBin).Length
Write-Host "Kernel build complete: $kernelBin ($kernelSize bytes)" -ForegroundColor Green
Write-Host ""

Write-Host "[2/2] Building runner..." -ForegroundColor Yellow
& cargo build -p mfk-runner --release
if ($LASTEXITCODE -ne 0) {
    Write-Host "ERROR: Runner build failed" -ForegroundColor Red
    exit 1
}
Write-Host "Runner build complete: target/release/mfk-runner" -ForegroundColor Green
Write-Host ""

Write-Host "======================================"
Write-Host "Build Complete! ($buildMode)" -ForegroundColor Green
Write-Host "======================================"
Write-Host ""
Write-Host "Next, run the kernel:"
Write-Host "  .\run.ps1 target/x86_64-mfk/$buildMode/mfk-kernel"
