# Hyper-V Setup Helper for Windows (Generation 2 / UEFI)
# Run in PowerShell as Administrator.
param(
    [switch]$SkipSwitchCheck = $false
)

Write-Host "======================================"
Write-Host "MFK Hyper-V Setup Helper (Windows)"
Write-Host "======================================"
Write-Host ""

$fail = $false

# [1/5] Admin check (Hyper-V cmdlets need elevation).
Write-Host "[1/5] Checking Administrator privileges..."
$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if ($isAdmin) {
    Write-Host "  OK: running elevated" -ForegroundColor Green
} else {
    Write-Host "  ERROR: not elevated. Right-click PowerShell -> Run as administrator." -ForegroundColor Red
    $fail = $true
}
Write-Host ""

# [2/5] Hyper-V role / module.
Write-Host "[2/5] Checking Hyper-V role..."
try {
    $feature = Get-WindowsOptionalFeature -Online -FeatureName Microsoft-Hyper-V -ErrorAction SilentlyContinue
    if ($feature -and $feature.State -eq "Enabled") {
        Write-Host "  OK: Microsoft-Hyper-V = Enabled" -ForegroundColor Green
    } else {
        Write-Host "  ERROR: Hyper-V role not enabled (state: $($feature.State))." -ForegroundColor Red
        Write-Host "  Enable: OptionalFeatures.exe -> Hyper-V -> reboot." -ForegroundColor Yellow
        $fail = $true
    }
} catch {
    Write-Host "  WARNING: could not query Hyper-V feature: $_" -ForegroundColor Yellow
}
if (-not (Get-Module -ListAvailable -Name Hyper-V)) {
    Write-Host "  ERROR: Hyper-V PowerShell module not found." -ForegroundColor Red
    $fail = $true
} else {
    Write-Host "  OK: Hyper-V PowerShell module present" -ForegroundColor Green
}
if (-not (Get-Command New-VM -ErrorAction SilentlyContinue)) {
    Write-Host "  ERROR: New-VM cmdlet unavailable." -ForegroundColor Red
    $fail = $true
}
Write-Host ""

# [3/5] Virtual switches (Default Switch + custom).
if (-not $SkipSwitchCheck) {
    Write-Host "[3/5] Available virtual switches:"
    try {
        $switches = @(Get-VMSwitch -ErrorAction Stop)
        if ($switches.Count -eq 0) {
            Write-Host "  ERROR: no virtual switch found." -ForegroundColor Red
            Write-Host "  Create: Hyper-V Manager -> Virtual Switch Manager, or pass --hyperv-switch=`"<name>`"." -ForegroundColor Yellow
            $fail = $true
        } else {
            foreach ($sw in $switches) {
                $marker = if ($sw.Name -eq "Default Switch") { "(default)" } else { "" }
                Write-Host "  - $($sw.Name) [$($sw.SwitchType)] $marker" -ForegroundColor Cyan
            }
            if (-not ($switches | Where-Object { $_.Name -eq "Default Switch" })) {
                Write-Host "  NOTE: 'Default Switch' missing; runner uses first switch unless --hyperv-switch is given." -ForegroundColor Yellow
            }
        }
    } catch {
        Write-Host "  ERROR: Get-VMSwitch failed: $_" -ForegroundColor Red
        $fail = $true
    }
    Write-Host ""
} else {
    Write-Host "[3/5] Skipped switch check (-SkipSwitchCheck)." -ForegroundColor Gray
    Write-Host ""
}

# [4/5] qemu-img (needed for raw -> VHDX conversion; Convert-VHD is fallback).
Write-Host "[4/5] Checking disk conversion tools..."
$qemuImg = Get-Command qemu-img -ErrorAction SilentlyContinue
if ($qemuImg) {
    Write-Host "  OK: qemu-img at $($qemuImg.Source)" -ForegroundColor Green
} else {
    Write-Host "  NOTE: qemu-img not found; runner falls back to Convert-VHD." -ForegroundColor Yellow
    Write-Host "  Install: choco install qemu  (or download qemu-utils)" -ForegroundColor Gray
}
Write-Host ""

# [5/5] Existing MFK VMs.
Write-Host "[5/5] Existing MFK Hyper-V VMs:"
try {
    $vms = @(Get-VM -ErrorAction SilentlyContinue | Where-Object { $_.Name -like "MFK-*-uefi" })
    if ($vms.Count -eq 0) {
        Write-Host "  None found (created on first run)." -ForegroundColor Cyan
    } else {
        foreach ($vm in $vms) {
            Write-Host "  - $($vm.Name) [Gen$($vm.Generation), $($vm.State)]" -ForegroundColor Cyan
        }
    }
} catch {
    Write-Host "  Could not list VMs: $_" -ForegroundColor Yellow
}
Write-Host ""

# VBox conflict note (both hypervisors need the CPU hypervisor mode).
$vbox = Get-Command VBoxManage -ErrorAction SilentlyContinue
if ($vbox) {
    Write-Host "NOTE: VirtualBox is also installed. Hyper-V and VirtualBox can coexist" -ForegroundColor Yellow
    Write-Host "on modern Windows, but only one hypervisor runs a VM at a time." -ForegroundColor Yellow
    Write-Host ""
}

Write-Host "======================================"
if ($fail) {
    Write-Host "Setup INCOMPLETE - fix the errors above." -ForegroundColor Red
    exit 1
}
Write-Host "Setup Complete!" -ForegroundColor Green
Write-Host "======================================"
Write-Host ""
Write-Host "Next steps (elevated shell):"
Write-Host "  .\\build.ps1"
Write-Host "  .\\run.ps1 --hyperv --uefi"
Write-Host ""
Write-Host "With explicit resources:"
Write-Host "  cargo run -p mfk-runner --release -- target/x86_64-mfk/debug/mfk-kernel --hyperv ``"
Write-Host "    --hyperv-switch=`"Default Switch`" --hyperv-mem=512 --hyperv-cpus=2"
Write-Host ""
Write-Host "Serial output (named pipe):"
Write-Host "  .\\tools\\hyperv-serial.ps1"
Write-Host ""
Write-Host "See HYPERV_SETUP.md for details."
