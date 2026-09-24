# MFK Run Script for PowerShell
# Runs the kernel in VirtualBox or QEMU

param(
    [string]$KernelPath = "target/x86_64-mfk/debug/mfk-kernel",
    [ValidateSet("--vbox", "--virtualbox", "--qemu", "")]
    [string]$Hypervisor = "--vbox"
)

# Handle positional arguments
if ($args.Count -gt 0) {
    if ($args[0] -match "^(--qemu|--vbox|--virtualbox)$") {
        $Hypervisor = $args[0]
    }
    else {
        $KernelPath = $args[0]
    }
}

# Handle second positional argument
if ($args.Count -gt 1) {
    if ($args[1] -match "^(--qemu|--vbox|--virtualbox)$") {
        $Hypervisor = $args[1]
    }
}

Write-Host "Running MFK Kernel..." -ForegroundColor Cyan
Write-Host "  Kernel: $KernelPath" -ForegroundColor Gray
Write-Host "  Hypervisor: $($Hypervisor -replace '^--', '')" -ForegroundColor Gray
Write-Host ""

# Run the runner
$runnerCmd = @(
    "run",
    "-p", "mfk-runner",
    "--bin", "mfk-runner",
    "--release",
    "--",
    $KernelPath,
    $Hypervisor
)

& cargo $runnerCmd
if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Host "ERROR: Runner failed" -ForegroundColor Red
    exit 1
}
