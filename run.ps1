# MFK Run Script for PowerShell
# Runs the kernel in VirtualBox, QEMU, or Hyper-V (UEFI-only, Generation 2)

param(
    [string]$KernelPath = "target/x86_64-mfk/debug/mfk-kernel",
    [ValidateSet("--vbox", "--virtualbox", "--qemu", "--hyperv", "--hyper-v", "--hv", "")]
    [string]$Hypervisor = "--vbox"
)

# Collect passthrough flags for the runner (--uefi, --hyperv-boot=..., ...)
$RunnerExtra = @()
foreach ($a in $args) {
    if ($a -match "^(--qemu|--vbox|--virtualbox|--hyperv|--hyper-v|--hv)$") {
        if ($Hypervisor -eq "--vbox" -or $Hypervisor -eq "") { $Hypervisor = $a }
    }
    elseif ($a -match "^(--uefi|--bios|--no-run|--force|--hyperv-.*|--vhdx=.*|--vnc.*|--web-ui.*|--kbd=.*|--xhci-kbd|--bundle-apps|--with-apps|--data-disk-size=.*|--extra-disk.*|--boot-extra-disk.*|--gpu-.*)$") {
        $RunnerExtra += $a
    }
    else {
        $KernelPath = $a
    }
}

if ($Hypervisor -eq "--hyper-v" -or $Hypervisor -eq "--hv") { $Hypervisor = "--hyperv" }

# Hyper-V Generation 2 is UEFI-only.
if ($Hypervisor -eq "--hyperv" -and $RunnerExtra -contains "--bios") {
    Write-Host "ERROR: --hyperv is UEFI-only (Generation 2); drop --bios or pass --uefi." -ForegroundColor Red
    exit 1
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
) + $RunnerExtra

& cargo $runnerCmd
if ($LASTEXITCODE -ne 0) {
    Write-Host ""
    Write-Host "ERROR: Runner failed" -ForegroundColor Red
    exit 1
}
