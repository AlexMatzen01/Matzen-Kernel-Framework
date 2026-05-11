# MFK Installation Script for PowerShell (Windows)
# Sets up Rust nightly and required components for building the kernel

Write-Host "======================================"
Write-Host "MFK Installation Setup (Windows)"
Write-Host "======================================"
Write-Host ""

# Step 1: Check if Rust is installed
Write-Host "[1/3] Checking Rust installation..." -ForegroundColor Yellow

$rustVersion = & rustc --version 2>$null
if ($LASTEXITCODE -eq 0) {
    Write-Host "✓ Rust installed: $rustVersion" -ForegroundColor Green
}
else {
    Write-Host "ERROR: Rust not found" -ForegroundColor Red
    Write-Host ""
    Write-Host "Please install Rust from: https://rustup.rs/" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "Or use Chocolatey:"
    Write-Host "  choco install rust"
    Write-Host ""
    exit 1
}
Write-Host ""

# Step 2: Install nightly toolchain
Write-Host "[2/3] Installing/updating nightly toolchain..." -ForegroundColor Yellow

$nightlyCheck = & rustup show | Select-String "nightly"
if ($nightlyCheck) {
    Write-Host "✓ Nightly toolchain already installed" -ForegroundColor Green
}
else {
    Write-Host "Installing nightly..." -ForegroundColor Cyan
    & rustup toolchain install nightly
    if ($LASTEXITCODE -ne 0) {
        Write-Host "ERROR: Failed to install nightly" -ForegroundColor Red
        exit 1
    }
    Write-Host "✓ Nightly toolchain installed" -ForegroundColor Green
}
Write-Host ""

# Step 3: Add required components
Write-Host "[3/3] Adding required components..." -ForegroundColor Yellow

$componentsToAdd = @(
    "rust-src",
    "llvm-tools-preview"
)

foreach ($component in $componentsToAdd) {
    Write-Host "  Adding $component..." -ForegroundColor Gray
    $componentCheck = & rustup component list | Select-String "^$component.*\(installed\)"
    
    if ($componentCheck) {
        Write-Host "  ✓ $component already installed"
    }
    else {
        & rustup component add $component --toolchain nightly
        if ($LASTEXITCODE -ne 0) {
            Write-Host "  ✗ Failed to add $component" -ForegroundColor Red
            exit 1
        }
        Write-Host "  ✓ $component added"
    }
}
Write-Host ""

# Step 4: Verify VirtualBox (recommended)
Write-Host "[Bonus] Checking VirtualBox installation..." -ForegroundColor Yellow

$vboxCheck = & VBoxManage --version 2>$null
if ($LASTEXITCODE -eq 0) {
    Write-Host "✓ VirtualBox found: $vboxCheck" -ForegroundColor Green
}
else {
    Write-Host "⚠ VirtualBox not found (recommended for better networking)" -ForegroundColor Yellow
    Write-Host "  Install from: https://www.virtualbox.org/wiki/Downloads" -ForegroundColor Gray
    Write-Host "  Or use Chocolatey:"
    Write-Host "    choco install virtualbox"
}
Write-Host ""

Write-Host "======================================"
Write-Host "Setup Complete!" -ForegroundColor Green
Write-Host "======================================"
Write-Host ""
Write-Host "Next steps:"
Write-Host "1. Build the project:"
Write-Host "   .\build.ps1"
Write-Host ""
Write-Host "2. Run the kernel:"
Write-Host "   .\run.ps1"
Write-Host ""
