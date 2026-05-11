# VirtualBox Setup Helper for Windows
# Run in PowerShell as Administrator

param(
    [switch]$SkipInstall = $false,
    [string]$NetworkInterface = $null
)

Write-Host "======================================"
Write-Host "MFK VirtualBox Setup Helper (Windows)"
Write-Host "======================================"
Write-Host ""

# Check if running as Administrator
$isAdmin = ([Security.Principal.WindowsIdentity]::GetCurrent()).Groups -contains 'S-1-5-32-544'
if (-not $isAdmin) {
    Write-Host "WARNING: This script should be run as Administrator for best results." -ForegroundColor Yellow
    Write-Host ""
}

# Step 1: Check VirtualBox Installation
Write-Host "[1/4] Checking VirtualBox installation..."
$vboxPath = "C:\Program Files\Oracle\VirtualBox"
$vboxmanagePath = Join-Path $vboxPath "VBoxManage.exe"

if (-not (Test-Path $vboxmanagePath)) {
    Write-Host "ERROR: VirtualBox not found at $vboxPath" -ForegroundColor Red
    Write-Host ""
    Write-Host "Please install VirtualBox from: https://www.virtualbox.org/wiki/Downloads"
    Write-Host ""
    Write-Host "Or use Chocolatey:"
    Write-Host "  choco install virtualbox"
    Write-Host ""
    exit 1
}

Write-Host "✓ VirtualBox found at: $vboxPath" -ForegroundColor Green
Write-Host ""

# Step 2: Verify VBoxManage works
Write-Host "[2/4] Verifying VBoxManage..."
$vboxVersion = & $vboxmanagePath --version
if ($LASTEXITCODE -eq 0) {
    Write-Host "✓ VBoxManage working: $vboxVersion" -ForegroundColor Green
} else {
    Write-Host "ERROR: VBoxManage failed" -ForegroundColor Red
    exit 1
}
Write-Host ""

# Step 3: List available network interfaces
Write-Host "[3/4] Available network interfaces:"
$output = & $vboxmanagePath list bridgedifs
if ($LASTEXITCODE -eq 0) {
    $interfaces = @()
    $currentIface = @{}
    
    foreach ($line in $output) {
        if ($line -match "^Name:") {
            if ($currentIface.Count -gt 0) {
                $interfaces += $currentIface
            }
            $currentIface = @{ Name = $line -replace "^Name:\s*" }
        }
        elseif ($line -match "^Status:" -and $currentIface) {
            $currentIface.Status = $line -replace "^Status:\s*"
        }
    }
    
    if ($currentIface.Count -gt 0) {
        $interfaces += $currentIface
    }
    
    if ($interfaces.Count -eq 0) {
        Write-Host $output
    } else {
        $index = 1
        foreach ($iface in $interfaces) {
            $status = if ($iface.Status -match "Up") { "✓" } else { "✗" }
            Write-Host "  $index. $status $($iface.Name) - $($iface.Status)"
            $index++
        }
    }
} else {
    Write-Host "Could not list network interfaces" -ForegroundColor Yellow
}
Write-Host ""

# Step 4: Check/Create existing VMs
Write-Host "[4/4] Existing MFK VMs:"
$vms = & $vboxmanagePath list vms | Select-String "MFK-"
if ($vms.Count -eq 0) {
    Write-Host "  No MFK VMs found (they will be created on first run)" -ForegroundColor Cyan
} else {
    $vms | ForEach-Object {
        Write-Host "  • $_" -ForegroundColor Cyan
    }
}
Write-Host ""

Write-Host "======================================"
Write-Host "Setup Complete!"
Write-Host "======================================"
Write-Host ""
Write-Host "Next steps:"
Write-Host "1. Build the kernel:"
Write-Host "   .\build.sh"
Write-Host ""
Write-Host "2. Run in VirtualBox:"
Write-Host "   .\run.sh"
Write-Host ""
Write-Host "3. Inside the kernel, configure network:"
Write-Host "   dhclient eth0    # or set static IP"
Write-Host "   ping 8.8.8.8"
Write-Host ""
Write-Host "For more information, see VIRTUALBOX_SETUP.md"
Write-Host ""
