# MFK Build Script for PowerShell
# Builds the kernel and runner on Windows

Write-Host "Building MFK Kernel Framework..." -ForegroundColor Cyan
Write-Host ""

# Step 1: Build the kernel
Write-Host "[1/2] Building kernel..." -ForegroundColor Yellow
$kernelCmd = @(
    "build",
    "-p", "mfk-kernel",
    "--target", "targets/x86_64-mfk.json",
    "-Zjson-target-spec",
    "-Zbuild-std=core,alloc",
    "-Zbuild-std-features=compiler-builtins-mem"
)

$kernelProcess = & cargo $kernelCmd
if ($LASTEXITCODE -ne 0) {
    Write-Host "ERROR: Kernel build failed" -ForegroundColor Red
    exit 1
}
Write-Host "✓ Kernel build complete" -ForegroundColor Green
Write-Host ""

# Step 2: Build the runner
Write-Host "[2/2] Building runner..." -ForegroundColor Yellow
$runnerCmd = @(
    "build",
    "-p", "mfk-runner",
    "--release"
)

$runnerProcess = & cargo $runnerCmd
if ($LASTEXITCODE -ne 0) {
    Write-Host "ERROR: Runner build failed" -ForegroundColor Red
    exit 1
}
Write-Host "✓ Runner build complete" -ForegroundColor Green
Write-Host ""

Write-Host "======================================"
Write-Host "Build Complete!" -ForegroundColor Green
Write-Host "======================================"
Write-Host ""
Write-Host "Next, run the kernel:"
Write-Host "  .\run.ps1"
Write-Host ""
